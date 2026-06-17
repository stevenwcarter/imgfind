# Slint Native GUI Migration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace imgfind's React + Axum web frontend with a native Slint desktop GUI (Search + lightbox) that links the existing library in-process, and delete the entire HTTP stack.

**Architecture:** Convert the single package into a 2-member Cargo workspace: the existing `imgfind` crate (lib + CLI + TUI, web stack removed) and a new `imgfind-gui` binary crate holding all Slint code. The GUI calls existing library functions (`SearchEngine::search_meta`, `get_or_generate_thumbnail`, `ClipEmbedder`) directly — no HTTP. Pure controller logic (view-state, pagination, model gate) is unit-tested; Slint UI is verified by running.

**Tech Stack:** Rust edition 2024, Slint 1.16 (`slint` + `slint-build`), `image` 0.25 (JPEG decode → Slint pixel buffer), `open` 5 (open original in OS viewer), `clap` 4, existing `imgfind` library (`rusqlite`/`r2d2`, `sqlite-vec`, `clipper`).

## Global Constraints

- Rust **edition = "2024"** for every crate; keep all workspace crates' editions equal.
- `slint-build` lives **only** in `imgfind-gui`'s `[build-dependencies]`, never in the root package — it cannot be feature-gated and must stay out of the core/CLI/TUI/`just test` build graph.
- Errors use `anyhow` with `Context`/`with_context` at every boundary (matches the codebase).
- Logging via `tracing`; `RUST_LOG` controls verbosity.
- DB rows store paths **relative to `Database.parent_dir`**; convert at every filesystem boundary with `relative_to_abs_path` / `RelativePath` / `AbsolutePath`.
- Search uses `SearchConfig::default()` (distance ≤ 1.3, max_k 100) and page size **80**, matching the deleted REST defaults; `has_more = returned_rows == limit`.
- Lints: `cargo clippy --workspace --all-targets` must be warning-free; `cargo fmt --all` clean.

---

## File Structure

**Root `imgfind` crate (modified):**
- `Cargo.toml` — add `[workspace] members = [".", "imgfind-gui"]`; remove web deps.
- `src/lib.rs` — remove `pub mod api/graphql/routes/context`.
- `src/main.rs` — remove `Serve` subcommand + `serve()` fn and web imports.
- Delete: `src/routes.rs`, `src/api/mod.rs`, `src/api/search.rs`, `src/graphql.rs`, `src/context.rs`, `site/` (whole directory).

**New `imgfind-gui` crate:**
- `imgfind-gui/Cargo.toml`
- `imgfind-gui/build.rs` — `slint_build::compile("ui/app.slint")`.
- `imgfind-gui/ui/app.slint` — MainWindow: search bar, results grid, lightbox overlay.
- `imgfind-gui/src/main.rs` — arg parse, open backend, spawn model load, wire callbacks, run.
- `imgfind-gui/src/state.rs` — pure `SearchState` machine + `ViewState` (unit-tested).
- `imgfind-gui/src/backend.rs` — `Backend`: DB + embedder + search/thumbnail/abs-path.
- `imgfind-gui/src/image_util.rs` — JPEG bytes → `slint::Image` (unit-tested where pure).

---

## Task 1: Workspace + `imgfind-gui` skeleton (empty window builds & runs)

**Files:**
- Modify: `Cargo.toml` (root, add `[workspace]`)
- Create: `imgfind-gui/Cargo.toml`, `imgfind-gui/build.rs`, `imgfind-gui/ui/app.slint`, `imgfind-gui/src/main.rs`

**Interfaces:**
- Produces: a buildable workspace member `imgfind-gui` with a generated `MainWindow` Slint type and a `main()` that shows it.

- [ ] **Step 1: Add the workspace table to the root manifest**

Add to the **top** of `Cargo.toml` (above `[package]`):

```toml
[workspace]
members = [".", "imgfind-gui"]
resolver = "2"
```

- [ ] **Step 2: Create `imgfind-gui/Cargo.toml`**

```toml
[package]
name = "imgfind-gui"
version = "0.1.0"
edition = "2024"

[dependencies]
imgfind = { path = ".." }
slint = "1.16"
image = "0.25"
open = "5"
clap = { version = "4.0", features = ["derive"] }
anyhow = "1.0"
tracing = "0.1"
tracing-subscriber = "0.3"

[build-dependencies]
slint-build = "1.16"
```

- [ ] **Step 3: Create `imgfind-gui/build.rs`**

```rust
fn main() {
    slint_build::compile("ui/app.slint").expect("Slint build failed");
}
```

- [ ] **Step 4: Create a minimal `imgfind-gui/ui/app.slint`**

```slint
export component MainWindow inherits Window {
    title: "imgfind";
    preferred-width: 1100px;
    preferred-height: 800px;
    Text {
        text: "imgfind";
        font-size: 24px;
    }
}
```

- [ ] **Step 5: Create `imgfind-gui/src/main.rs`**

```rust
slint::include_modules!();

fn main() -> anyhow::Result<()> {
    let window = MainWindow::new()?;
    window.run()?;
    Ok(())
}
```

- [ ] **Step 6: Build the whole workspace**

Run: `cargo build --workspace`
Expected: PASS — both `imgfind` and `imgfind-gui` compile. (Slint pulls a large tree on first build; that is expected.)

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock imgfind-gui/
git commit -m "feat(gui): 2-crate workspace + imgfind-gui Slint skeleton"
```

---

## Task 2: Delete the web stack from the core crate

**Files:**
- Delete: `src/routes.rs`, `src/api/mod.rs`, `src/api/search.rs`, `src/graphql.rs`, `src/context.rs`, `site/`
- Modify: `src/lib.rs`, `src/main.rs`, `Cargo.toml` (root)

**Interfaces:**
- Produces: a core crate with no HTTP surface; CLI keeps `index/search/metadata/tui/thumbnails/clean/status/config/completions` (no `serve`).

- [ ] **Step 1: Remove the deleted modules from `src/lib.rs`**

Delete these four lines from `src/lib.rs`:

```rust
pub mod api;
pub mod context;
pub mod graphql;
pub mod routes;
```

- [ ] **Step 2: Delete the web source files and the React app**

```bash
git rm -r src/api src/graphql.rs src/routes.rs src/context.rs site
```

- [ ] **Step 3: Remove the `Serve` subcommand and `serve()` from `src/main.rs`**

Delete the `Serve { dir, host, port }` variant from the `Commands` enum (around `src/main.rs:124`), its match arm (around `src/main.rs:279`, the `Commands::Serve { .. } => { … serve(...).await?; }` block), the entire `async fn serve(...)` (around `src/main.rs:320`), and the now-unused imports `use imgfind::context::GraphQLContext;` and `use imgfind::routes::app;`. Leave `imgfind::search::SearchEngine`, `imgfind::thumbnail::*`, and all other imports intact.

- [ ] **Step 4: Build the core crate to surface remaining web references**

Run: `cargo build -p imgfind`
Expected: PASS. If it fails, the error points to a leftover reference to a deleted module — remove that reference (do not re-add the module).

- [ ] **Step 5: Remove now-unused HTTP dependencies from root `Cargo.toml`**

Delete these dependency lines: `axum`, `juniper`, `juniper_axum`, `tower`, `tower-http`, `rust-embed`, `mime_guess`, `axum-extra`. Then rebuild:

Run: `cargo build -p imgfind`
Expected: PASS. If the compiler now reports `serde_json` or `base64` as the only user gone, also remove those; if anything still uses them, keep them. Do **not** guess — let the build decide.

- [ ] **Step 6: Run the core test suite and clippy**

Run: `cargo test -p imgfind && cargo clippy -p imgfind --all-targets`
Expected: PASS, zero clippy warnings. The `safe_join` tests were in the deleted `api/search.rs` and go away with it; all other tests remain green.

- [ ] **Step 7: Verify the CLI no longer offers `serve`**

Run: `cargo run -p imgfind -- --help`
Expected: help text lists subcommands **without** `serve`.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: delete web stack (Axum/GraphQL/REST/React) and serve subcommand"
```

---

## Task 3: Pure search-state machine (`state.rs`)

**Files:**
- Create: `imgfind-gui/src/state.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod state;`)

**Interfaces:**
- Produces:
  - `pub const PAGE_SIZE: usize = 80;`
  - `pub struct SearchResult { pub path: String, pub distance: f32, pub file_size: Option<i64> }`
  - `pub enum ViewState { Idle, Loading, Error, Empty, Results }`
  - `pub struct SearchState { … }` with: `new()`, `start_search(&mut self, query: String)`, `apply_page(&mut self, results: Vec<SearchResult>, offset: usize)`, `apply_error(&mut self, message: String, offset: usize)`, `next_offset(&self) -> usize`, `view_state(&self) -> ViewState`, and public read access to `results`, `has_more`, `error`, `committed_query`.

- [ ] **Step 1: Write the failing tests**

Create `imgfind-gui/src/state.rs`:

```rust
//! Pure, UI-agnostic search state machine. Mirrors the React app's
//! `searchViewState.ts` + pagination behavior so it can be unit-tested
//! without the Slint runtime.

pub const PAGE_SIZE: usize = 80;

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub path: String,
    pub distance: f32,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewState {
    Idle,
    Loading,
    Error,
    Empty,
    Results,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str) -> SearchResult {
        SearchResult { path: path.into(), distance: 0.1, file_size: Some(1024) }
    }

    #[test]
    fn fresh_state_is_idle() {
        let s = SearchState::new();
        assert_eq!(s.view_state(), ViewState::Idle);
    }

    #[test]
    fn during_search_is_loading() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        assert_eq!(s.view_state(), ViewState::Loading);
        assert_eq!(s.committed_query, "cat");
    }

    #[test]
    fn empty_results_after_search_is_empty() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![], 0);
        assert_eq!(s.view_state(), ViewState::Empty);
    }

    #[test]
    fn nonempty_results_is_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        assert_eq!(s.view_state(), ViewState::Results);
    }

    #[test]
    fn error_state_takes_precedence() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_error("boom".into(), 0);
        assert_eq!(s.view_state(), ViewState::Error);
        assert_eq!(s.error.as_deref(), Some("boom"));
    }

    #[test]
    fn full_page_sets_has_more() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        let page: Vec<SearchResult> = (0..PAGE_SIZE).map(|i| r(&format!("{i}.jpg"))).collect();
        s.apply_page(page, 0);
        assert!(s.has_more);
        assert_eq!(s.next_offset(), PAGE_SIZE);
    }

    #[test]
    fn short_page_clears_has_more() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        assert!(!s.has_more);
    }

    #[test]
    fn load_more_appends_not_replaces() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        s.apply_page(vec![r("b.jpg")], 1);
        assert_eq!(s.results.len(), 2);
        assert_eq!(s.results[1].path, "b.jpg");
    }

    #[test]
    fn fresh_search_replaces_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        s.start_search("dog".into());
        s.apply_page(vec![r("b.jpg")], 0);
        assert_eq!(s.results.len(), 1);
        assert_eq!(s.results[0].path, "b.jpg");
    }

    #[test]
    fn error_on_first_page_clears_results_but_keeps_old_on_load_more() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        // error while loading more (offset > 0) keeps existing results
        s.apply_error("net".into(), 1);
        assert_eq!(s.results.len(), 1);
        // error on a fresh search (offset 0) clears
        s.start_search("dog".into());
        s.apply_error("net".into(), 0);
        assert!(s.results.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind-gui state::`
Expected: FAIL — `cannot find type SearchState in this scope`.

- [ ] **Step 3: Implement `SearchState`**

Insert above the `#[cfg(test)]` block:

```rust
#[derive(Debug, Default)]
pub struct SearchState {
    pub committed_query: String,
    pub results: Vec<SearchResult>,
    pub loading: bool,
    pub error: Option<String>,
    pub has_more: bool,
    has_searched: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a fresh (offset 0) search for `query`.
    pub fn start_search(&mut self, query: String) {
        self.committed_query = query;
        self.loading = true;
        self.error = None;
        self.has_searched = true;
    }

    /// Apply a returned page. `offset == 0` replaces; `offset > 0` appends.
    pub fn apply_page(&mut self, mut results: Vec<SearchResult>, offset: usize) {
        self.has_more = results.len() == PAGE_SIZE;
        if offset == 0 {
            self.results = results;
        } else {
            self.results.append(&mut results);
        }
        self.loading = false;
        self.error = None;
    }

    /// Record a failure. A first-page (offset 0) failure clears results.
    pub fn apply_error(&mut self, message: String, offset: usize) {
        self.error = Some(message);
        self.loading = false;
        if offset == 0 {
            self.results.clear();
        }
    }

    pub fn next_offset(&self) -> usize {
        self.results.len()
    }

    pub fn view_state(&self) -> ViewState {
        if self.loading {
            ViewState::Loading
        } else if self.error.is_some() {
            ViewState::Error
        } else if !self.has_searched {
            ViewState::Idle
        } else if self.results.is_empty() {
            ViewState::Empty
        } else {
            ViewState::Results
        }
    }
}
```

Add `mod state;` to `imgfind-gui/src/main.rs` (below `slint::include_modules!();`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind-gui state::`
Expected: PASS (all 10 tests).

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/state.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): pure search-state machine (view-state + pagination)"
```

---

## Task 4: Backend service (`backend.rs`) — DB, embedder, search, thumbnails

**Files:**
- Create: `imgfind-gui/src/backend.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod backend;`)

**Interfaces:**
- Consumes: `state::{SearchResult, PAGE_SIZE}`; `imgfind::{get_db_path, relative_to_abs_path, RelativePath, AbsolutePath}`; `imgfind::database::Database`; `imgfind::search::SearchEngine`; `imgfind::config::SearchConfig`; `imgfind::thumbnail::get_or_generate_thumbnail`; `imgfind::models::ensure_and_activate_model`; `clipper::ClipEmbedder`.
- Produces:
  - `#[derive(Clone)] pub struct Backend` with:
    - `pub fn open(dir: Option<&str>) -> anyhow::Result<Backend>`
    - `pub fn start_loading_model(&self)` — spawns a thread that fills the embedder `OnceLock`.
    - `pub fn model_ready(&self) -> bool`
    - `pub fn search(&self, query: &str, offset: usize) -> anyhow::Result<Vec<SearchResult>>` (uses `PAGE_SIZE` as limit)
    - `pub fn thumbnail(&self, rel_path: &str, size: u32) -> anyhow::Result<Vec<u8>>`
    - `pub fn abs_path(&self, rel_path: &str) -> std::path::PathBuf`

- [ ] **Step 1: Write the failing tests**

Create `imgfind-gui/src/backend.rs`:

```rust
//! In-process data backend for the GUI. Wraps the imgfind library: opens the
//! SQLite DB, loads the CLIP embedder in the background, and runs searches /
//! thumbnail loads. No HTTP.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use imgfind::config::SearchConfig;
use imgfind::database::Database;
use imgfind::search::SearchEngine;
use imgfind::thumbnail::get_or_generate_thumbnail;
use imgfind::{RelativePath, get_db_path, relative_to_abs_path};

use crate::state::{PAGE_SIZE, SearchResult};

#[derive(Clone)]
pub struct Backend {
    db: Database,
    embedder: Arc<OnceLock<ClipEmbedder>>,
    parent_dir: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db() -> (Database, PathBuf) {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("imgfind_gui_test_{}_{n}", std::process::id()));
        let db_path = root.join(".imgfind").join("imgfind.db");
        let db = Database::new(&db_path).expect("create db");
        (db, root)
    }

    fn backend_with(db: Database) -> Backend {
        let parent_dir = db.parent_dir.clone();
        Backend { db, embedder: Arc::new(OnceLock::new()), parent_dir }
    }

    #[test]
    fn abs_path_joins_relative_onto_parent_dir() {
        let (db, root) = temp_db();
        let parent = db.parent_dir.clone();
        let backend = backend_with(db);
        assert_eq!(backend.abs_path("a/b.jpg"), parent.join("a/b.jpg"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn thumbnail_round_trips_through_relative_path() {
        let (db, root) = temp_db();
        // Insert an image row + a cached 300px thumbnail blob (cache hit, no file I/O).
        {
            let conn = db.pool.get().expect("conn");
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
                [],
            )
            .expect("insert image");
        }
        db.insert_thumbnail("h", 300, &[1, 2, 3, 4]).expect("insert thumb");

        let backend = backend_with(db);
        let bytes = backend.thumbnail("a.jpg", 300).expect("thumb");
        assert_eq!(bytes, vec![1, 2, 3, 4]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn model_not_ready_before_load() {
        let (db, root) = temp_db();
        let backend = backend_with(db);
        assert!(!backend.model_ready());
        let _ = std::fs::remove_dir_all(root);
    }
}
```

> Note: this test uses `db.pool` and `db.parent_dir`, which are public on `Database`. If `pool` is not accessible from the gui crate, insert the row via a public DB method instead; the round-trip assertion (rel path → thumbnail bytes) is the load-bearing check and must stay.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind-gui backend::`
Expected: FAIL — methods `abs_path` / `thumbnail` / `model_ready` not found.

- [ ] **Step 3: Implement `Backend`**

Insert above the `#[cfg(test)]` block:

```rust
impl Backend {
    pub fn open(dir: Option<&str>) -> Result<Backend> {
        let db_path = get_db_path(dir).context("Failed to resolve image database")?;
        let db = Database::new(&db_path).context("Failed to open database")?;
        let parent_dir = db.parent_dir.clone();
        Ok(Backend { db, embedder: Arc::new(OnceLock::new()), parent_dir })
    }

    /// Build the CLIP embedder on a background thread (it can take seconds and
    /// must not block the UI). Mirrors `serve`'s lazy init.
    pub fn start_loading_model(&self) {
        let embedder = Arc::clone(&self.embedder);
        let db = self.db.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<ClipEmbedder> {
                let active = imgfind::models::ensure_and_activate_model(&db, None)
                    .context("Failed to resolve active embedding model")?;
                ClipEmbedder::from_model(&active, false)
                    .context("Failed to load CLIP model")
            })();
            match result {
                Ok(e) => {
                    let _ = embedder.set(e);
                }
                Err(err) => tracing::error!("model load failed: {err:#}"),
            }
        });
    }

    pub fn model_ready(&self) -> bool {
        self.embedder.get().is_some()
    }

    pub fn search(&self, query: &str, offset: usize) -> Result<Vec<SearchResult>> {
        let embedder = self
            .embedder
            .get()
            .context("Embedding model is still loading")?;
        let embedding = embedder
            .get_text_embedding(query)
            .context("Failed to embed query")?;
        let sc = SearchConfig::default();
        let engine = SearchEngine::new(&self.db);
        let rows = engine
            .search_meta(embedding, PAGE_SIZE, offset, sc.distance_threshold, sc.max_k)
            .context("Search failed")?;
        Ok(rows
            .into_iter()
            .map(|(path, distance, file_size)| SearchResult { path, distance, file_size })
            .collect())
    }

    pub fn thumbnail(&self, rel_path: &str, size: u32) -> Result<Vec<u8>> {
        let hash = self
            .db
            .get_image_hash(&RelativePath(PathBuf::from(rel_path)))
            .with_context(|| format!("No hash for {rel_path}"))?;
        get_or_generate_thumbnail(&self.db, rel_path, &hash, size)
            .with_context(|| format!("Failed to load thumbnail for {rel_path}"))
    }

    pub fn abs_path(&self, rel_path: &str) -> PathBuf {
        relative_to_abs_path(std::path::Path::new(rel_path), &self.parent_dir)
    }
}
```

Verify the exact signature of `imgfind::models::ensure_and_activate_model` (the second arg here is the optional requested model name, `None` ⇒ use the active/default). If its signature differs, adapt the call but keep the behavior: resolve & activate the model, return its name. Add `mod backend;` to `imgfind-gui/src/main.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind-gui backend::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/backend.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): in-process backend (db, bg model load, search, thumbnails)"
```

---

## Task 5: Search UI — bar + results grid wired to backend (model-loading state)

**Files:**
- Modify: `imgfind-gui/ui/app.slint`, `imgfind-gui/src/main.rs`
- Create: `imgfind-gui/src/image_util.rs`

**Interfaces:**
- Consumes: `Backend`, `SearchState`, `ViewState`.
- Produces (Slint, in `app.slint`): `struct Tile { path: string, image: image, size-kb: int }`; on `MainWindow`: `in property <[Tile]> tiles`, `in property <string> status`, `in property <bool> show-load-more`, `in property <bool> can-search`, `callback search(string)`, `callback load-more()`, `callback tile-clicked(int)`, `callback tile-open-external(int)`.
- Produces (Rust): `image_util::jpeg_to_slint_image(bytes: &[u8]) -> anyhow::Result<slint::Image>`.

- [ ] **Step 1: Write the failing test for the image decoder**

Create `imgfind-gui/src/image_util.rs`:

```rust
//! Decode stored JPEG thumbnail bytes into a Slint image.

use anyhow::{Context, Result};
use slint::{Image, SharedPixelBuffer};

pub fn jpeg_to_slint_image(bytes: &[u8]) -> Result<Image> {
    let rgba = image::load_from_memory(bytes)
        .context("Failed to decode image bytes")?
        .to_rgba8();
    let (w, h) = rgba.dimensions();
    let buffer = SharedPixelBuffer::clone_from_slice(rgba.as_raw(), w, h);
    Ok(Image::from_rgba8(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 red PNG (image crate decodes PNG and JPEG alike); proves bytes
    /// become a non-empty Slint image rather than panicking.
    #[test]
    fn decodes_valid_image_bytes() {
        // 1x1 red pixel, encoded as PNG via the image crate at test time.
        let mut png: Vec<u8> = Vec::new();
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let slint_img = jpeg_to_slint_image(&png).expect("decode");
        assert_eq!(slint_img.size().width, 1);
    }

    #[test]
    fn rejects_garbage_bytes() {
        assert!(jpeg_to_slint_image(&[0, 1, 2, 3]).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p imgfind-gui image_util::`
Expected: FAIL — module/function not found.

- [ ] **Step 3: Wire `mod image_util;` and confirm the test passes**

Add `mod image_util;` to `imgfind-gui/src/main.rs`.

Run: `cargo test -p imgfind-gui image_util::`
Expected: PASS (2 tests).

- [ ] **Step 4: Build the search UI in `app.slint`**

Replace `imgfind-gui/ui/app.slint` with:

```slint
import { LineEdit, Button, ScrollView } from "std-widgets.slint";

export struct Tile {
    path: string,
    image: image,
    size-kb: int,
}

export component MainWindow inherits Window {
    title: "imgfind";
    preferred-width: 1100px;
    preferred-height: 800px;
    background: #1f2430;

    in property <[Tile]> tiles;
    in property <string> status;
    in property <bool> show-load-more;
    in property <bool> can-search: false;

    callback search(string);
    callback load-more();
    callback tile-clicked(int);
    callback tile-open-external(int);

    VerticalLayout {
        padding: 16px;
        spacing: 12px;

        HorizontalLayout {
            spacing: 8px;
            query := LineEdit {
                placeholder-text: root.can-search ? "Search images..." : "Loading model...";
                enabled: root.can-search;
                accepted(text) => { root.search(text); }
            }
        }

        Text {
            text: root.status;
            color: #9aa4b2;
            visible: root.status != "";
        }

        ScrollView {
            VerticalLayout {
                spacing: 12px;
                grid := HorizontalLayout {
                    // Simple wrapping grid via Flow is approximated with a
                    // GridLayout fed in the Rust side; here we render a flow of tiles.
                }
                for tile[i] in root.tiles: Rectangle {
                    height: 200px;
                    width: 200px;
                    border-radius: 8px;
                    clip: true;
                    Image {
                        source: tile.image;
                        image-fit: cover;
                        width: 100%;
                        height: 100%;
                    }
                    TouchArea {
                        clicked => { root.tile-clicked(i); }
                        pointer-event(e) => {
                            if (e.button == PointerEventButton.right) {
                                root.tile-open-external(i);
                            }
                        }
                    }
                }

                if root.show-load-more: Button {
                    text: "Load more";
                    clicked => { root.load-more(); }
                }
            }
        }
    }
}
```

> Slint layout note: a true CSS-column masonry is out of scope (per spec). The `for` loop above renders fixed 200px tiles; if they stack vertically instead of wrapping, wrap the `for` in Slint's flow/grid construct available in 1.16 (e.g. lay tiles into rows in the Rust model, or use a `GridLayout` with a computed column count). Getting a clean wrapping grid is part of this task's deliverable; the exact Slint construct is the implementer's choice as long as tiles wrap into rows.

- [ ] **Step 5: Wire the controller in `main.rs`**

Implement, in `imgfind-gui/src/main.rs`: parse `--dir`, `Backend::open`, `backend.start_loading_model()`, build `MainWindow`. Use a `slint::Timer` (repeating, ~250ms) to poll `backend.model_ready()` and set `can-search` + `status` ("Loading model…" → "" once ready). Hold `SearchState` in an `Rc<RefCell<…>>`. On `search(query)`: if empty, reset to idle; else `state.start_search(query)`, set status "Searching…", spawn a thread that calls `backend.search(&q, 0)`, and marshal the result back with `slint::invoke_from_event_loop` + a `Weak<MainWindow>` — on the UI thread call `state.apply_page`/`apply_error`, then rebuild the `tiles` model (decode each thumbnail via `image_util::jpeg_to_slint_image`, skipping/placeholdering failures), set `status` from `view_state()`, and `show-load-more` from `state.has_more`. `load-more()` does the same with `state.next_offset()`. `tile-open-external(i)` calls `open::that(backend.abs_path(&path))`.

Reference for the marshalling pattern:

```rust
let weak = window.as_weak();
let backend2 = backend.clone();
std::thread::spawn(move || {
    let res = backend2.search(&q, offset);
    let weak = weak.clone();
    slint::invoke_from_event_loop(move || {
        if let Some(w) = weak.upgrade() {
            // borrow state, apply_page/apply_error, rebuild tiles model, set props
        }
    })
    .ok();
});
```

- [ ] **Step 6: Build and smoke-test**

Run: `cargo build -p imgfind-gui`
Expected: PASS.

Then, against a directory that already has an indexed DB:
Run: `cargo run -p imgfind-gui -- --dir <indexed-dir>`
Expected (manual): window opens; shows "Loading model…", then the search box enables; typing a query + Enter shows a wrapping grid of thumbnails; "Load more" appears when a full page returns and appends on click; right-click opens the original in the OS viewer.

- [ ] **Step 7: Commit**

```bash
git add imgfind-gui/
git commit -m "feat(gui): search bar + thumbnail grid wired to in-process backend"
```

---

## Task 6: Lightbox overlay (full image, prev/next, esc, zoom, open original)

**Files:**
- Modify: `imgfind-gui/ui/app.slint`, `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes: tiles + backend full-image loading.
- Produces (Slint): on `MainWindow`: `in property <bool> lightbox-open`, `in property <image> lightbox-image`, `callback lightbox-close()`, `callback lightbox-prev()`, `callback lightbox-next()`. Reuses `tile-clicked(int)` to open.

- [ ] **Step 1: Add the lightbox overlay to `app.slint`**

Append inside `MainWindow` (after the main `VerticalLayout`), so it overlays:

```slint
    // --- Lightbox overlay ---
    if root.lightbox-open: Rectangle {
        background: #000000dd;
        TouchArea { clicked => { root.lightbox-close(); } }

        Image {
            source: root.lightbox-image;
            image-fit: contain;
            width: 90%;
            height: 90%;
            x: (parent.width - self.width) / 2;
            y: (parent.height - self.height) / 2;
        }

        // Navigation
        HorizontalLayout {
            width: 100%;
            height: 100%;
            Button { text: "‹"; clicked => { root.lightbox-prev(); } }
            Rectangle { } // spacer
            Button { text: "›"; clicked => { root.lightbox-next(); } }
        }

        forward-focus: key-handler;
        key-handler := FocusScope {
            key-pressed(event) => {
                if (event.text == Key.Escape) { root.lightbox-close(); return accept; }
                if (event.text == Key.LeftArrow) { root.lightbox-prev(); return accept; }
                if (event.text == Key.RightArrow) { root.lightbox-next(); return accept; }
                return reject;
            }
        }
    }
```

> Zoom: Slint 1.16's `Image` does not scroll-to-zoom out of the box. For v1, implement zoom by binding the image `width`/`height` to a `zoom` factor property changed on scroll inside a `TouchArea { scroll-event(e) => { … } }`, or accept fit-to-window with prev/next as the v1 deliverable and note scroll-zoom as a follow-up. Fit-to-window + keyboard nav is the minimum acceptable deliverable; scroll-zoom is a bonus.

- [ ] **Step 2: Wire lightbox state in `main.rs`**

Add a current-index field alongside `SearchState`. On `tile-clicked(i)`: set the index, load the **full-size** image bytes via `std::fs::read(backend.abs_path(&path))`, decode with `image_util::jpeg_to_slint_image` (full-res), set `lightbox-image`, set `lightbox-open = true`. `lightbox-prev`/`lightbox-next`: clamp index to `0..tiles.len()`, reload the image at the new index. `lightbox-close`: set `lightbox-open = false`. Load full images off the UI thread (same `invoke_from_event_loop` pattern) so large files don't freeze the window.

- [ ] **Step 3: Build and smoke-test**

Run: `cargo build -p imgfind-gui`
Expected: PASS.

Run: `cargo run -p imgfind-gui -- --dir <indexed-dir>`
Expected (manual): clicking a thumbnail opens a full-screen overlay with the full image; ‹/› and Left/Right arrows navigate; Esc and clicking the backdrop close it.

- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/
git commit -m "feat(gui): lightbox overlay with prev/next and keyboard nav"
```

---

## Task 7: Docs, installer, and final workspace verification

**Files:**
- Modify: `CLAUDE.md`, `README.md`, `USAGE.md`, `install.sh`

**Interfaces:** none (docs + verification).

- [ ] **Step 1: Update `CLAUDE.md`**

In the "What this is" / "Architecture" sections: change "three frontends from one binary" to "a core `imgfind` binary (CLI + TUI) plus an `imgfind-gui` binary (native Slint GUI)"; delete the Web-server (`serve`), GraphQL, REST `/api/v1`, SPA-fallback, and map-clustering-via-GraphQL bullets and the React frontend section; delete the "Critical build-order gotcha" (`yarn build` before `cargo build`) paragraph; add a short "Workspace" note (`imgfind` + `imgfind-gui`, run the GUI with `cargo run -p imgfind-gui -- [--dir DIR]`) and a one-line pointer to this spec.

- [ ] **Step 2: Update `README.md` and `USAGE.md`**

Remove `serve` and the web UI from command lists; add `imgfind-gui` (native search GUI). Note that the map view is not yet ported to the GUI.

- [ ] **Step 3: Update `install.sh` to install both binaries**

After the existing `imgfind` copy, build/copy the GUI binary too:

```bash
GUI_BINARY="$PROJECT_DIR/target/release/imgfind-gui"
if [ -f "$GUI_BINARY" ]; then
    echo "📦 Installing imgfind-gui to $LOCAL_BIN..."
    cp "$GUI_BINARY" "$LOCAL_BIN/imgfind-gui"
    chmod +x "$LOCAL_BIN/imgfind-gui"
fi
```

Update the build hint near the top from `cargo build --release` to `cargo build --release --workspace`.

- [ ] **Step 4: Full workspace verification**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --release --workspace
```
Expected: fmt clean; clippy zero warnings; all tests pass; release build of both binaries succeeds.

- [ ] **Step 5: Smoke-test both frontends**

```bash
cargo run -p imgfind -- --help        # no `serve`; CLI intact
cargo run -p imgfind -- status        # core still works against a DB
cargo run -p imgfind-gui -- --dir <indexed-dir>   # GUI search + lightbox
```
Expected: CLI help lacks `serve`; GUI launches and searches.

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md README.md USAGE.md install.sh
git commit -m "docs: document Slint GUI + workspace; drop web server from docs/installer"
```

---

## Self-Review

**Spec coverage:**
- 2-crate workspace, slint-build isolation → Task 1, Global Constraints. ✓
- Delete web stack (files, subcommand, deps) → Task 2. ✓
- In-process data surface → Task 4 (uses the exact library functions named in the spec). ✓
- Search page: Enter-to-search, grid, Load-more, view states → Tasks 3 (logic) + 5 (UI). ✓
- Lightbox: full image, prev/next, Esc, zoom (best-effort), open original → Tasks 5 (open external) + 6. ✓
- Model loading state (background, gated search) → Tasks 4 + 5. ✓
- View-state port of `searchViewState.ts` → Task 3. ✓
- Pagination/has_more rule, relative-path invariant round-trip → Tasks 3 + 4 (tests). ✓
- Docs/installer updates → Task 7. ✓
- Map deferred, tags/collections out, masonry simplified → reflected as scope notes, no tasks (correct). ✓

**Placeholder scan:** No "TBD"/"handle edge cases"-style gaps; UI-construct latitude in Tasks 5/6 is bounded by an explicit "minimum acceptable deliverable" rather than left open. ✓

**Type consistency:** `SearchResult`/`PAGE_SIZE` defined in Task 3, consumed unchanged in Task 4; `Backend` methods named in Task 4 match their uses in Tasks 5/6; Slint `Tile`/callback names consistent between Tasks 5 and 6. ✓

**Risk flag for the implementer:** Slint API specifics (exact `scroll-event`, `PointerEventButton`, `FocusScope` key handling, wrapping-grid construct) may differ slightly in 1.16 — consult Slint 1.16 docs (via context7) when a `.slint` snippet doesn't compile; the behavior contract, not the exact snippet, is what must hold.
