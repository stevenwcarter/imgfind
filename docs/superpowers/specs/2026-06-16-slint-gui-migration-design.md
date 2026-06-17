# Slint native GUI migration — design

**Date:** 2026-06-16
**Branch:** `slint-migration`
**Status:** Approved (brainstorming → spec)

## Summary

Replace imgfind's web frontend (React SPA + Axum HTTP server) with a native
**Slint** desktop GUI that links the existing library **in-process**. The GUI
mimics the current web **Search** experience (text query → thumbnail grid →
lightbox). The interactive Leaflet **Map** view is **deferred** (dropped from
both React and the new GUI for now). The entire HTTP stack is **deleted**.

End state: one core binary (`imgfind` — CLI + TUI) plus a new GUI binary
(`imgfind-gui`), all data access in-process against the local SQLite DB. No
HTTP, no GraphQL, no REST, no embedded SPA.

## Decisions (settled during brainstorming)

1. **Data access:** in-process native. The GUI calls library functions
   directly; no HTTP server backs it.
2. **Server fate:** delete the Axum server, GraphQL, REST API, and the `serve`
   subcommand entirely.
3. **Map view:** defer. v1 GUI ships Search + lightbox only; the React map is
   deleted too. A native map is a future, separate project.
4. **Packaging:** minimal 2-crate Cargo workspace. The current package becomes a
   workspace member retaining lib + CLI + TUI; a new `imgfind-gui` binary crate
   holds all Slint code and its build tooling. Rationale: Slint compiles
   `.slint` files via the `slint-build` **build-dependency**, which cannot be
   feature-gated — in a single crate it would compile on every `cargo build`/
   `cargo test`/`just test`. A separate crate keeps it out of the core build
   graph. (This is intentionally *much* smaller than the originally-floated
   server-crate/shared-crate split, which was abandoned.)

## Architecture

### Workspace layout

To minimize churn, the existing package stays at the repo root and its
`Cargo.toml` gains a `[workspace]` table (a package can be its own workspace
root). No existing source files move.

```
imgfind/                  # repo root: the existing `imgfind` package
├── Cargo.toml            # [package] imgfind  +  [workspace] members = [".", "imgfind-gui"]
├── src/                  # unchanged location: lib + CLI + TUI (web stack removed)
└── imgfind-gui/          # NEW member: Slint binary
    ├── Cargo.toml        # depends on imgfind via { path = ".." }
    ├── build.rs
    ├── src/
    └── ui/*.slint
```

**Invariant:** a two-member workspace where `imgfind-gui` depends on `imgfind`
by path and Slint's build tooling (`slint-build`) lives **only** in
`imgfind-gui`, never in the root package's manifest. (This is intentionally
*much* smaller than the originally-floated server-crate/shared-crate split,
which was abandoned.)

### In-process data surface (already exists — no new library API required)

- `imgfind::get_db_path(dir)` — resolve DB (walk-up / global fallback).
- `imgfind::database::Database::new(&db_path)` — pooled handle; `parent_dir` is
  the base for relative→absolute path conversion.
- `clipper::ClipEmbedder::from_model(active_name, false)` +
  `imgfind::models::ensure_and_activate_model` — construct the embedder;
  `get_text_embedding(&str) -> Vec<f32>`.
- `imgfind::search::SearchEngine::new(&db).search_meta(embedding, limit, offset,
  distance_threshold, max_k) -> Vec<(String /*rel path*/, f32 /*distance*/,
  Option<i64> /*file_size*/)>` — the same call the deleted REST `/search` used.
- `imgfind::config::SearchConfig::default()` — distance ≤ 1.3, max_k 100.
- `imgfind::thumbnail::get_or_generate_thumbnail(&db, rel_path, &hash, size)` +
  `db.get_image_hash(&RelativePath(..))` — size-300 JPEG bytes for grid/lightbox.
- Full-size image bytes: read the file at
  `imgfind::relative_to_abs_path(rel_path, &db.parent_dir)`.

### GUI structure (`imgfind-gui`)

- `main.rs` — clap arg parse (`--dir` optional, mirroring `tui`), resolve DB,
  open `Database`, spawn the background model-load thread, build the Slint
  `MainWindow`, wire callbacks, run the event loop.
- `build.rs` — `slint_build::compile("ui/app.slint")`.
- `ui/*.slint` — declarative UI: search bar, results grid, lightbox overlay.
- A Rust **state/controller module** holding the testable logic (see Testing).
- Search runs off the UI thread (the embedding + sqlite-vec query can take time);
  results are marshalled back to the UI thread via Slint's
  `invoke_from_event_loop` / a weak handle, mirroring how the TUI uses channels.

## Data flow

1. Startup: resolve DB → open `Database` → spawn thread that builds the
   `ClipEmbedder` (background, like `serve`'s lazy init). UI starts in a
   **"loading model…"** state; the search control is disabled/queued until the
   embedder is ready.
2. User types a query, presses **Enter**.
3. Controller embeds the query, calls `search_meta(.., limit=80, offset=0, ..)`.
4. For each result row, load the size-300 thumbnail blob; populate the grid model.
5. `has_more = rows.len() == limit`; the **"Load more"** button is shown when
   true. Clicking it re-queries with `offset = current_count` and **appends**.
6. Clicking a thumbnail opens the **lightbox overlay**: full-size image, prev/
   next (arrow keys + on-screen buttons), Esc to close, scroll-to-zoom.
7. Ctrl-click / double-click a thumbnail → **open the original file in the OS
   default viewer** (native analog of the web "open original"/download).

## View states (port of `site/src/page/searchViewState.ts`)

`idle | loading | error | empty | results`, selected by the same priority:
loading → error → (!searched ⇒ idle) → (count == 0 ⇒ empty) → results. This is
ported as a pure Rust function with unit tests.

## What gets deleted

- `site/` — the entire React app (and its `site/build` embedded assets).
- root `imgfind` crate: `src/routes.rs`, `src/api/` (mod + search),
  `src/graphql.rs`, `src/context.rs`; the `Serve` subcommand and `serve()` fn in
  `main.rs`; their `pub mod` lines in `lib.rs`.
- Dependencies that become unused: `axum`, `juniper`, `juniper_axum`, `tower`,
  `tower-http`, `rust-embed`, `mime_guess`, `axum-extra`. Prune `serde_json`,
  `base64`, `serde` features only if no remaining code uses them (verified by
  build + `cargo machete`/clippy, not assumed).
- The build-order gotcha (`yarn build` before `cargo build`) disappears.

## Error handling

- DB not found → friendly message in the GUI (and non-zero exit if it can't
  open at all), reusing `get_db_path`'s existing errors.
- Embedder load failure → surfaced as the `error` view state with the message;
  search stays disabled.
- Search failure → `error` view state; previous results cleared only on a fresh
  (offset 0) search, matching the React behavior.
- Missing/undecodable thumbnail → placeholder tile; never crash the grid.
- All boundaries use `anyhow` `Context`, consistent with the codebase.

## Testing (TDD)

Slint view rendering is not unit-tested. The **pure logic is**, written
test-first:

1. **View-state selector** — port of `searchViewState.ts` with the same cases
   (idle/loading/error/empty/results), including the precedence rules.
2. **Pagination** — `has_more = rows.len() == limit`; next-offset = current
   result count; append-not-replace on "Load more"; replace-on-fresh-search.
3. **Model-load gate** — search is rejected/queued while the embedder is absent
   and enabled once present.
4. **Path/thumbnail helpers** — any rel→abs conversion or thumbnail-key logic
   added in the GUI crate (the library's own are already tested).

Per the project's spec discipline: do not skip a test by appealing to "the
library already guarantees X." The search→results→pagination flow is pinned by
tests at the controller seam, not trusted via prose. The library's existing
tests (search, thumbnails, path conversion) continue to run unchanged.

**Regression guard for the deletion:** after removing the web stack, the core
crate must still build, `cargo test` must pass, and the CLI + TUI must function.
The plan includes a verification task that builds both crates, runs clippy
(zero warnings) and the full test suite, and smoke-tests `imgfind search` and
`imgfind-gui`.

## Invariants this feature depends on

- **Relative-path storage:** DB rows hold paths relative to `Database.parent_dir`.
  The GUI converts at every filesystem boundary via `relative_to_abs_path` /
  `RelativePath`, exactly as the deleted REST layer did. A test exercises a
  thumbnail+full-image load round-trip through this conversion.
- **Embedding dimension is per-model and L2-normalized** before search; the GUI
  reuses `SearchEngine` (which normalizes) rather than embedding+searching by
  hand, so it inherits this invariant.
- **`search_meta` distance/k semantics** (`distance ≤ 1.3`, `k` ceiling 100)
  come from `SearchConfig::default()`; the GUI uses the same defaults so results
  match the old web behavior.

## Out of scope (v1)

- The interactive map view (deferred; `get_images_by_bounds` stays in the
  library, unused by the GUI for now).
- Tags/collections UI (the React app never surfaced them).
- Search-as-you-type / debounce (web required Enter; GUI matches).
- True CSS-column masonry layout (v1 uses a responsive wrapping/aspect grid).
- Save-as / download-to-path (replaced by "open original in OS viewer").

## Documentation updates (part of this branch)

- `CLAUDE.md`: "three frontends from one binary" → core binary (CLI + TUI) +
  `imgfind-gui` binary; remove the web-server/GraphQL/REST/SPA architecture
  sections and the `yarn build` build-order gotcha; document the workspace and
  how to run the GUI.
- `README.md` / `USAGE.md`: drop `serve`, document `imgfind-gui`.
- `install.sh`: install both binaries.
- `justfile`: unchanged (test loop still works; faster now).
```
