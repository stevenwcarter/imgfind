# Code-Health Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the user-selected subset of the `bughunt.md` audit — path-traversal hardening, localhost-default binding, CLIP-model caching, removal of the DB mutex bottleneck, and five panic/correctness fixes — without rewrites.

**Architecture:** Each finding is an isolated, small change with its own commit. Server-wiring changes are ordered P2 → P1 → C3 → S2 so `src/context.rs` and `serve()` evolve sequentially and the build stays green at every step. TUI/lib/api findings are independent and come first.

**Tech Stack:** Rust (edition 2024), Axum 0.8, Juniper, rusqlite + r2d2 + sqlite-vec, ratatui, the local `clipper` crate. Verify with `cargo build`, `cargo clippy --all-targets`, `cargo test`.

**Source audit:** `bughunt.md`. Every task strips the fixed finding's section from `bughunt.md` in the same commit (strip-on-fix rule).

**Commit convention:** `fix(<area>): <summary> [<finding-id>]`.

---

## Task 1: C2 — `current_dir().unwrap()` → `?`

**Files:**
- Modify: `src/lib.rs:31`

- [ ] **Step 1: Apply the fix**

In `src/lib.rs`, replace line 31:

```rust
    let mut current_dir = std::env::current_dir().unwrap();
```

with:

```rust
    let mut current_dir = std::env::current_dir().context("Failed to get current directory")?;
```

(`Context` is already imported via `use anyhow::{Context, Result};` at the top of the file.)

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles cleanly.

- [ ] **Step 3: Strip finding from bughunt.md**

Remove the `### C2 — current_dir().unwrap()` heading and its body from `bughunt.md` (under CORRECTNESS).

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs bughunt.md
git commit -m "fix(cli): propagate current_dir error instead of panicking [C2]"
```

---

## Task 2: C1 — `get_db_path(Some(dir))` returns Err instead of panic

**Files:**
- Modify: `src/lib.rs:21-29`

- [ ] **Step 1: Apply the fix**

In `src/lib.rs`, replace this block:

```rust
    if let Some(dir) = dir {
        let potential_db = Path::new(&dir).join(".imgfind").join("imgfind.db");
        if potential_db.exists() {
            return Ok(potential_db);
        } else {
            panic!("No database found in this directory")
        }
    }
```

with:

```rust
    if let Some(dir) = dir {
        let potential_db = Path::new(&dir).join(".imgfind").join("imgfind.db");
        if potential_db.exists() {
            return Ok(potential_db);
        } else {
            return Err(anyhow::anyhow!("No database found in directory: {dir}"));
        }
    }
```

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles cleanly.

- [ ] **Step 3: Strip finding from bughunt.md**

Remove the `### C1 — get_db_path(Some(dir)) panics...` heading and body from `bughunt.md`.

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs bughunt.md
git commit -m "fix(cli): return error when no db found in given dir [C1]"
```

---

## Task 3: C4 — Focus-index underflow on empty image list

**Files:**
- Modify: `src/tui/app/focus.rs:24-29` (function head)
- Test: `src/tui/app/focus.rs` (existing `#[cfg(test)] mod tests` — ADD a new test, do not modify existing tests)

- [ ] **Step 1: Add the failing test**

In the `mod tests` block of `src/tui/app/focus.rs`, add this new test (leave all existing tests untouched):

```rust
    #[test]
    fn it_handles_empty_image_list_without_panicking() {
        assert_eq!(calculate_new_focus_index(0, 0, Left), 0);
        assert_eq!(calculate_new_focus_index(0, 0, Right), 0);
        assert_eq!(calculate_new_focus_index(0, 0, Up), 0);
        assert_eq!(calculate_new_focus_index(0, 0, Down), 0);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --bin imgfind -- focus::tests::it_handles_empty_image_list_without_panicking`
Expected: FAIL — panics with `attempt to subtract with overflow` (or `divide by zero`).

(If the `--bin` filter doesn't match, run `cargo test it_handles_empty_image_list` and confirm that test panics.)

- [ ] **Step 3: Apply the fix**

In `calculate_new_focus_index`, add an early return at the very top of the function body (before `let mut current_index = current_index;`):

```rust
pub fn calculate_new_focus_index(
    images_len: u8,
    current_index: u8,
    direction: FocusDirection,
) -> u8 {
    if images_len == 0 {
        return 0;
    }
    let mut current_index = current_index;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test it_handles_empty_image_list`
Expected: PASS. Also run the full focus suite: `cargo test focus::` → all existing focus tests still PASS.

- [ ] **Step 5: Strip finding from bughunt.md**

Remove the `### C4 — TUI focus underflow...` heading and body from `bughunt.md`.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app/focus.rs bughunt.md
git commit -m "fix(tui): guard focus index against empty image list [C4]"
```

---

## Task 4: C5 — Graceful errors in the zoom path

**Files:**
- Modify: `src/tui/app/zoom.rs:4` (import), `:30-34`, `:65-95`

- [ ] **Step 1: Widen the tracing import**

In `src/tui/app/zoom.rs`, change line 4:

```rust
use tracing::debug;
```

to:

```rust
use tracing::{debug, warn};
```

- [ ] **Step 2: Replace the `.expect("image not found")` lookup**

Replace this block (currently around lines 30-37):

```rust
            if let Some(zoom_index) = zoom {
                let image_entry = self
                    .images
                    .get(zoom_index as usize)
                    .expect("image not found");
                let image_path = image_entry.path.clone();
                let image_score = image_entry.score;
```

with:

```rust
            if let Some(zoom_index) = zoom {
                let Some(image_entry) = self.images.get(zoom_index as usize) else {
                    warn!("zoom requested for missing image index {zoom_index}");
                    return;
                };
                let image_path = image_entry.path.clone();
                let image_score = image_entry.score;
```

- [ ] **Step 3: Replace the open/decode `.expect()` calls inside the spawned task**

Replace this block (currently around lines 67-76):

```rust
                    let base_image = if Some(&image_path) == zoomed_path.as_ref()
                        && let Some(zoomed_image) = zoomed_image
                    {
                        zoomed_image
                    } else {
                        ImageReader::open(image_path.clone())
                            .expect("could not open")
                            .decode()
                            .expect("could not decoded")
                    };
```

with:

```rust
                    let base_image = if Some(&image_path) == zoomed_path.as_ref()
                        && let Some(zoomed_image) = zoomed_image
                    {
                        zoomed_image
                    } else {
                        match ImageReader::open(&image_path) {
                            Ok(reader) => match reader.decode() {
                                Ok(img) => img,
                                Err(e) => {
                                    warn!("failed to decode image {image_path}: {e}");
                                    return;
                                }
                            },
                            Err(e) => {
                                warn!("failed to open image {image_path}: {e}");
                                return;
                            }
                        }
                    };
```

- [ ] **Step 4: Replace the `.expect()` on channel send**

Replace this block (currently around lines 92-94):

```rust
                    zoom_tx
                        .send(image_entry)
                        .expect("Could not send image entry");
```

with:

```rust
                    if let Err(e) = zoom_tx.send(image_entry) {
                        debug!("zoom receiver dropped, discarding image entry: {e}");
                    }
```

- [ ] **Step 5: Build + clippy**

Run: `cargo build && cargo clippy --all-targets`
Expected: compiles; no new warnings about unused `warn`/`debug`.

- [ ] **Step 6: Strip finding from bughunt.md**

Remove the `### C5 — TUI zoom path...` heading and body from `bughunt.md`.

- [ ] **Step 7: Commit**

```bash
git add src/tui/app/zoom.rs bughunt.md
git commit -m "fix(tui): handle zoom image open/decode/send errors gracefully [C5]"
```

---

## Task 5: S1 — Path-traversal containment in the `file` endpoint

**Files:**
- Modify: `src/api/search.rs` (imports, `file` handler, add `safe_join` helper + tests)

- [ ] **Step 1: Add the failing tests for the helper**

At the bottom of `src/api/search.rs`, add a test module (the helper `safe_join` does not exist yet):

```rust
#[cfg(test)]
mod tests {
    use super::safe_join;
    use std::path::{Path, PathBuf};

    #[test]
    fn allows_simple_relative() {
        let base = Path::new("/srv/images");
        assert_eq!(
            safe_join(base, "a/b.jpg"),
            Some(PathBuf::from("/srv/images/a/b.jpg"))
        );
    }

    #[test]
    fn allows_internal_dotdot() {
        let base = Path::new("/srv/images");
        assert_eq!(
            safe_join(base, "a/../b.jpg"),
            Some(PathBuf::from("/srv/images/b.jpg"))
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        let base = Path::new("/srv/images");
        assert_eq!(safe_join(base, "../../etc/passwd"), None);
    }

    #[test]
    fn rejects_absolute_path() {
        let base = Path::new("/srv/images");
        assert_eq!(safe_join(base, "/etc/passwd"), None);
    }

    #[test]
    fn rejects_climb_then_descend_escape() {
        let base = Path::new("/srv/images");
        assert_eq!(safe_join(base, "a/../../b"), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --bin imgfind safe_join`
Expected: FAIL — `cannot find function safe_join`.

- [ ] **Step 3: Add the `safe_join` helper**

Add this function to `src/api/search.rs` (e.g. above the `routes` function):

```rust
/// Join `filename` onto `base`, returning the path only if it stays within `base`.
/// Rejects absolute paths and any `..` that would climb above `base`.
fn safe_join(base: &std::path::Path, filename: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;

    let rel = std::path::Path::new(filename);
    if rel.is_absolute() {
        return None;
    }

    let mut result = base.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => result.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() || !result.starts_with(base) {
                    return None;
                }
            }
            // RootDir / Prefix => absolute or platform prefix: reject.
            _ => return None,
        }
    }

    if result.starts_with(base) {
        Some(result)
    } else {
        None
    }
}
```

- [ ] **Step 4: Run helper tests to verify they pass**

Run: `cargo test --bin imgfind safe_join`
Expected: all 5 PASS.

- [ ] **Step 5: Update imports**

In `src/api/search.rs`, the current top import line is:

```rust
use axum::{Extension, Json, Router, extract::Path, response::IntoResponse, routing::get};
```

Change it to add `Response` and `StatusCode`:

```rust
use axum::{
    Extension, Json, Router,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
```

And change the tracing import line:

```rust
use tracing::{debug, error};
```

to:

```rust
use tracing::{debug, error, warn};
```

- [ ] **Step 6: Rewrite the `file` handler to use `safe_join`**

Replace the entire `file` handler (currently lines ~64-79):

```rust
async fn file(
    Extension(context): Extension<GraphQLContext>,
    Path(filename): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let full_path = std::path::Path::new(&context.basepath).join(&filename);
    debug!("Filename: {}", filename);
    debug!("Full path: {:?}", full_path);

    match std::fs::read(&full_path) {
        Ok(data) => Ok(data),
        Err(e) => {
            error!("Error reading file {}: {}", filename, e);
            Err(e)?
        }
    }
}
```

with:

```rust
async fn file(
    Extension(context): Extension<GraphQLContext>,
    Path(filename): Path<String>,
) -> Response {
    // Canonicalize the base so containment checks compare real paths.
    let base = std::fs::canonicalize(&context.basepath)
        .unwrap_or_else(|_| std::path::PathBuf::from(&context.basepath));

    let Some(full_path) = safe_join(&base, &filename) else {
        warn!("rejected path traversal attempt: {filename}");
        return StatusCode::NOT_FOUND.into_response();
    };
    debug!("Serving file: {:?}", full_path);

    match std::fs::read(&full_path) {
        Ok(data) => data.into_response(),
        Err(e) => {
            error!("Error reading file {}: {}", filename, e);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}
```

Note: the route registration `.route("/file/{*filename}", get(file))` is unchanged — `Response` implements `IntoResponse`. The `AppError` import may now be unused by `file` but is still used by `thumb`/`search`, so leave the import.

- [ ] **Step 7: Build + clippy + tests**

Run: `cargo build && cargo clippy --all-targets && cargo test --bin imgfind safe_join`
Expected: compiles; 5 tests PASS.

- [ ] **Step 8: Strip finding from bughunt.md**

Remove the `### S1 — Path traversal...` heading and body from `bughunt.md` (under SECURITY).

- [ ] **Step 9: Commit**

```bash
git add src/api/search.rs bughunt.md
git commit -m "fix(api): contain file endpoint to basepath, reject traversal [S1]"
```

---

## Task 6: P2 — Remove the `Arc<Mutex<Database>>` bottleneck

This change spans five files and must land as **one commit** so the build stays green.

**Files:**
- Modify: `src/context.rs` (drop `Arc<Mutex>`, drop `get_db`)
- Modify: `src/database.rs` (`insert_thumbnail`: `&mut self` → `&self`)
- Modify: `src/thumbnail.rs` (`get_or_generate_thumbnail`: `db: &mut Database` → `db: &Database`)
- Modify: `src/graphql.rs` (`images_by_bounds`: drop `.lock()`)
- Modify: `src/api/search.rs` (`thumb`, `search`: drop `.lock()`)

- [ ] **Step 1: Rewrite `src/context.rs`**

Replace the whole file with:

```rust
use crate::database::Database;

#[derive(Clone)]
pub struct GraphQLContext {
    pub db: Database,
    pub basepath: String,
}

impl GraphQLContext {
    pub fn new(db: Database, basepath: String) -> Self {
        GraphQLContext { db, basepath }
    }
}

impl juniper::Context for GraphQLContext {}
```

- [ ] **Step 2: Relax `Database::insert_thumbnail` to `&self`**

In `src/database.rs`, change the `insert_thumbnail` receiver:

```rust
    pub fn insert_thumbnail(
        &mut self,
        image_hash: &str,
        size: u32,
        thumbnail_data: &[u8],
    ) -> Result<()> {
```

to:

```rust
    pub fn insert_thumbnail(
        &self,
        image_hash: &str,
        size: u32,
        thumbnail_data: &[u8],
    ) -> Result<()> {
```

(The body only calls `self.pool.get()`, which needs `&self`.)

- [ ] **Step 3: Relax `get_or_generate_thumbnail` to `&Database`**

In `src/thumbnail.rs`, change the signature:

```rust
pub fn get_or_generate_thumbnail(
    db: &mut Database,
    filepath: &str,
    hash: &str,
    size: u32,
) -> Result<Vec<u8>> {
```

to:

```rust
pub fn get_or_generate_thumbnail(
    db: &Database,
    filepath: &str,
    hash: &str,
    size: u32,
) -> Result<Vec<u8>> {
```

(The body calls `db.get_thumbnail` (`&self`) and `db.insert_thumbnail` (now `&self`) — no other change needed.)

- [ ] **Step 4: Drop the lock in `src/graphql.rs`**

In `images_by_bounds`, replace:

```rust
        let db = context.get_db().await;
        let db = db.lock().unwrap();

        let (images, original_count) = db.get_images_by_bounds(north, south, east, west)?;
```

with:

```rust
        let db = &context.db;

        let (images, original_count) = db.get_images_by_bounds(north, south, east, west)?;
```

(The later `db.get_thumbnail(&image.hash, 300)` call is unchanged and works on `&Database`.)

- [ ] **Step 5: Drop the locks in `src/api/search.rs`**

In `thumb`, replace:

```rust
    let mut db = context.db.lock().unwrap();

    // Get the image hash from the database
    let hash = db
        .get_image_hash(&filename)
        .with_context(|| format!("Failed to get hash for image: {}", filename))?;

    // Generate or retrieve thumbnail
    let thumbnail_bytes = get_or_generate_thumbnail(&mut db, &filename, &hash, size)?;
```

with:

```rust
    let db = &context.db;

    // Get the image hash from the database
    let hash = db
        .get_image_hash(&filename)
        .with_context(|| format!("Failed to get hash for image: {}", filename))?;

    // Generate or retrieve thumbnail
    let thumbnail_bytes = get_or_generate_thumbnail(db, &filename, &hash, size)?;
```

In `search`, replace:

```rust
    let db = context.db.lock().unwrap();
    let search = SearchEngine::new(&db);
```

with:

```rust
    let search = SearchEngine::new(&context.db);
```

- [ ] **Step 6: Build + clippy + test**

Run: `cargo build && cargo clippy --all-targets && cargo test`
Expected: compiles cleanly (no `unused import: Mutex`/`Arc` warnings from context.rs); all tests pass.

- [ ] **Step 7: Strip finding from bughunt.md**

Remove the `### P2 — Arc<Mutex<Database>>...` heading and body from `bughunt.md`.

- [ ] **Step 8: Commit**

```bash
git add src/context.rs src/database.rs src/thumbnail.rs src/graphql.rs src/api/search.rs bughunt.md
git commit -m "fix(server): drop DB mutex; use pooled connections per request [P2]"
```

---

## Task 7: P1 — Cache the CLIP model in `GraphQLContext`

**Files:**
- Modify: `src/context.rs` (add `embedder` field)
- Modify: `src/main.rs` (`serve`: build embedder once)
- Modify: `src/api/search.rs` (`search`: reuse `context.embedder`)

- [ ] **Step 1: Add the `embedder` field to the context**

Rewrite `src/context.rs` to:

```rust
use crate::database::Database;
use clipper::ClipEmbedder;
use std::sync::Arc;

#[derive(Clone)]
pub struct GraphQLContext {
    pub db: Database,
    pub basepath: String,
    pub embedder: Arc<ClipEmbedder>,
}

impl GraphQLContext {
    pub fn new(db: Database, basepath: String, embedder: Arc<ClipEmbedder>) -> Self {
        GraphQLContext {
            db,
            basepath,
            embedder,
        }
    }
}

impl juniper::Context for GraphQLContext {}
```

- [ ] **Step 2: Build the embedder once in `serve`**

In `src/main.rs`, add `use std::sync::Arc;` to the imports (near the other `use std::...` lines).

Then in `serve`, replace:

```rust
async fn serve(db: Database, directory: String, port: usize) -> Result<()> {
    // Placeholder for future server implementation
    let context = GraphQLContext::new(db, directory);
```

with:

```rust
async fn serve(db: Database, directory: String, port: usize) -> Result<()> {
    info!("Loading CLIP model...");
    let embedder =
        Arc::new(ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?);
    let context = GraphQLContext::new(db, directory, embedder);
```

(`info`, `ClipEmbedder`, and `Context` are already imported in `main.rs`. The stale "Placeholder" comment is intentionally removed here; the remaining `serve` body is finalized in Task 8/C3.)

- [ ] **Step 3: Reuse the cached embedder in the search handler**

In `src/api/search.rs`, replace the body start of `search`:

```rust
async fn search(
    Extension(context): Extension<GraphQLContext>,
    Path(search): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;

    // Generate embedding for query
    let query_embedding = model
        .get_text_embedding(search.as_str())
        .context("Failed to generate text embedding")?;
    let normalized_query = normalize_vector(&query_embedding);
```

with:

```rust
async fn search(
    Extension(context): Extension<GraphQLContext>,
    Path(search): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Generate embedding for query using the cached model.
    let query_embedding = context
        .embedder
        .get_text_embedding(search.as_str())
        .context("Failed to generate text embedding")?;
    let normalized_query = normalize_vector(&query_embedding);
```

- [ ] **Step 4: Remove the now-unused `ClipEmbedder` import in search.rs**

In `src/api/search.rs`, delete the line:

```rust
use clipper::ClipEmbedder;
```

(It is no longer referenced in this file. Verify with clippy in the next step.)

- [ ] **Step 5: Build + clippy + test**

Run: `cargo build && cargo clippy --all-targets && cargo test`
Expected: compiles cleanly with no unused-import warnings.

**If the build fails with a `Send`/`Sync` error** on `Arc<ClipEmbedder>` inside `Extension`/the async handler: wrap the embedding call in `tokio::task::spawn_blocking`, cloning the `Arc` into the closure:

```rust
    let embedder = context.embedder.clone();
    let query = search.clone();
    let query_embedding = tokio::task::spawn_blocking(move || embedder.get_text_embedding(&query))
        .await
        .context("embedding task panicked")?
        .context("Failed to generate text embedding")?;
```

Do not change anything else if this fallback is needed.

- [ ] **Step 6: Strip finding from bughunt.md**

Remove the `### P1 — CLIP model reloaded...` heading and body from `bughunt.md`.

- [ ] **Step 7: Commit**

```bash
git add src/context.rs src/main.rs src/api/search.rs bughunt.md
git commit -m "fix(server): load CLIP model once and share via context [P1]"
```

---

## Task 8: C3 — `serve()` error propagation + remove stale comment

**Files:**
- Modify: `src/main.rs` (`serve`: replace `.unwrap()`/`.expect()` on bind/serve with `?`)

- [ ] **Step 1: Apply the fix**

In `src/main.rs`, replace the bind/serve block in `serve` (currently):

```rust
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap();
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    });

    server.await.expect("Server failed to start");

    Ok(())
```

with:

```rust
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;
    info!("Listening on http://{addr}");
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    });

    server.await.context("Server error")?;

    Ok(())
```

(The Ctrl-C handler's `.expect()` stays — it runs inside the shutdown future where returning an error isn't available; a failure there is unrecoverable anyway. The stale "Placeholder" comment was already removed in Task 7.)

- [ ] **Step 2: Build + clippy**

Run: `cargo build && cargo clippy --all-targets`
Expected: compiles cleanly.

- [ ] **Step 3: Strip finding from bughunt.md**

Remove the `### C3 — serve() panics on bind/serve...` heading and body from `bughunt.md`.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs bughunt.md
git commit -m "fix(server): propagate serve bind/run errors instead of panicking [C3]"
```

---

## Task 9: S2 — Default-bind to `127.0.0.1`, opt-in `--host`

**Files:**
- Modify: `src/main.rs` (`Serve` subcommand: add `host`; call site; `serve` signature + bind)

- [ ] **Step 1: Add the `host` arg to the `Serve` subcommand**

In `src/main.rs`, the `Serve` variant currently is:

```rust
    Serve {
        #[arg(short, long)]
        dir: Option<String>,
        #[arg(short, long, default_value_t = 6060)]
        port: usize,
    },
```

Change it to:

```rust
    Serve {
        #[arg(short, long)]
        dir: Option<String>,
        /// Address to bind. Use 0.0.0.0 to expose on all interfaces.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value_t = 6060)]
        port: usize,
    },
```

- [ ] **Step 2: Update the call site**

In the `match cli.command` block, replace:

```rust
        Commands::Serve { dir, port } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = Database::new(&db_path)?;
            serve(db, dir.unwrap_or(".".to_owned()), port).await?;
        }
```

with:

```rust
        Commands::Serve { dir, host, port } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = Database::new(&db_path)?;
            serve(db, dir.unwrap_or(".".to_owned()), host, port).await?;
        }
```

- [ ] **Step 3: Update `serve` signature and bind address**

Change the `serve` signature:

```rust
async fn serve(db: Database, directory: String, port: usize) -> Result<()> {
```

to:

```rust
async fn serve(db: Database, directory: String, host: String, port: usize) -> Result<()> {
```

And change the address line (from Task 8):

```rust
    let addr = format!("0.0.0.0:{port}");
```

to:

```rust
    let addr = format!("{host}:{port}");
```

- [ ] **Step 4: Build + clippy + test**

Run: `cargo build && cargo clippy --all-targets && cargo test`
Expected: compiles cleanly; all tests pass.

- [ ] **Step 5: Manual smoke check of the CLI surface**

Run: `cargo run -- serve --help`
Expected: help text lists `--host <HOST>` with default `127.0.0.1` and `--port`.

- [ ] **Step 6: Strip finding from bughunt.md**

Remove the `### S2 — Server binds 0.0.0.0...` heading and body from `bughunt.md`.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs bughunt.md
git commit -m "fix(server): default bind to 127.0.0.1 with opt-in --host [S2]"
```

---

## Final verification

- [ ] Run the full suite once more: `cargo build && cargo clippy --all-targets && cargo test` — expect green.
- [ ] Confirm `bughunt.md` no longer contains S1, S2, P1, P2, C1, C2, C3, C4, C5 (only the out-of-scope findings P3, P4, C6, C7, D1–D8, O1–O3, R1, R2 remain).
- [ ] No summary commit — the per-finding commits are the audit trail.
