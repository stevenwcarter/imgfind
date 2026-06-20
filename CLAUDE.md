# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`imgfind` is a CLIP-based semantic image search tool. It indexes images into a local SQLite database (storing 512- or 768-dim CLIP embeddings depending on the selected model) and lets you find them by natural-language query.

The project ships as a **2-crate Cargo workspace**: a core `imgfind` binary (CLI + ratatui TUI) and a separate `imgfind-gui` binary (native Slint desktop GUI). See `docs/superpowers/specs/2026-06-16-slint-gui-migration-design.md` for the migration design.

## Workspace

| Crate | Binary | Description |
|-------|--------|-------------|
| `imgfind` (root) | `imgfind` | Library + CLI + TUI (`tui` feature) |
| `imgfind-gui` | `imgfind-gui` | Native Slint desktop GUI |

## Build & run

```bash
cargo build --release --workspace   # build both binaries
./install.sh                        # copies both binaries to ~/.local/bin

cargo test --workspace              # Rust tests (both crates)
cargo test config::                 # run a single module's tests (e.g. config)
just test                           # watchexec-driven test loop (cargo test on rs/toml change)
just cover                          # coverage via cargo llvm-cov -> lcov.info
```

Run the native GUI:
```bash
imgfind gui [-d DIR]                     # installed: launcher subcommand, forwards args to imgfind-gui
cargo run -p imgfind-gui -- [--dir DIR]  # or run the GUI crate directly
# -d/--dir resolves the DB via the same walk-up/global logic as the CLI
```

## CLI commands

`index` (default recursive), `search`, `metadata`, `tui`, `gui`, `thumbnails`, `clean`, `status`, `config {show|add-ignore|remove-ignore|reset}`. See `USAGE.md` and `src/main.rs` for flags. Notable: `index --root` forces a DB in the cwd; `search --short` prints bare paths for piping; `search --display`/`tui` render images inline in supporting terminals; `gui [ARGS]` is a passthrough launcher (`Commands::Gui`, `trailing_var_arg`) that spawns `imgfind-gui` with `ARGS` (block model, exit-code propagated; binary resolved sibling-of-`current_exe` then PATH). `thumbnails` now accepts repeatable `--size <N>` and a `--gui-sizes` flag that expands to the canonical GUI sizes (300, 512, 2048); use `--gui-sizes` to pre-generate thumbnails so first-view decoding is instant.

## Architecture

Library code lives in `src/` and is exposed via `src/lib.rs`; `src/main.rs` is the CLI entrypoint that dispatches subcommands.

- **Database resolution (`src/lib.rs`)** — `get_db_path` walks *up* the directory tree from cwd looking for `.imgfind/imgfind.db`, falling back to `~/.imgfind/imgfind.db`. `get_db_path(Some(dir))` **panics** if no DB exists under `dir`. `get_local_db_path` creates one in cwd (used by `index --root`).

- **Relative path invariant** — Image paths are stored in the DB **relative to the DB's parent directory** (the dir containing `.imgfind`), not as absolute paths. `abs_to_relative_path` / `relative_to_abs_path` in `lib.rs` convert at every boundary. When adding queries, remember rows hold relative paths; convert before any filesystem access. `Database.parent_dir` holds this base.

- **Storage (`src/database.rs`)** — SQLite via `rusqlite` + an `r2d2` connection pool (`Database` is `Clone`, shares the pool). The `sqlite-vec` extension is auto-loaded in `Database::new`. Schema is managed by a `PRAGMA user_version`-gated migration runner (`run_migrations`, called on every `Database::new`): migration 001 creates the baseline schema, 002 adds the `models`/`user_data` tables, 003 adds `ui_state`. The runner is idempotent (each migration uses `IF NOT EXISTS` and bumps `user_version` only after success); `LATEST_MIGRATION_VERSION = 3`. (`diesel`/`diesel_migrations` are in `Cargo.toml` but unused — the live schema is managed entirely by this runner.) Tables:
  - `images` (id, relative path, oshash, created_at)
  - `image_vectors` — `vec0` virtual table, `float[512]`, rowid = image id. Search is `embedding MATCH ? AND k=N AND distance <= 1.3 ORDER BY distance`.
  - `thumbnails` — JPEG blobs keyed by `(image_hash, size)`. GUI thumbnail sizes: `GUI_THUMBNAIL_SIZES = [300, 512, 2048]` (grid / detail panel / lightbox). `LIGHTBOX_SIZE = 2048`. All three are persisted via `get_or_generate_thumbnail` so repeat views cost nothing.
  - `image_metadata` — EXIF: dimensions, GPS lat/long, camera make/model, datetime.
  - `ui_state` — single-row JSON session state (`id = 1` CHECK constraint); holds search text, sort, filters, ordered result id list, selection, detail-panel state, and scroll position; also holds tag-support state (`brushes`, `recent_tags`, `rail_visible`, `Filters.tags`/`tag_match`/`tags_enabled`). Restored on next launch without re-running the query.
  - `tags` (id, name UNIQUE, created_at) and `image_tags` (image_id, tag_id, cascade-on-delete FK) — baseline schema (migration 001); tag support uses these without any additional migration.

- **Embeddings** — Generated by the `clipper` crate, a **local path dependency at `../clipper`** (sibling repo, must be present to build). `clipper` is multi-model: it vendors candle's CLIP module (with a config-driven `Gelu` activation, needed for LAION which uses gelu) and exposes a catalog via `clipper::supported_models()` plus `ClipEmbedder::from_model(name, use_cpu)`. The default model is `openai/clip-vit-base-patch32` (512-dim); `laion/CLIP-ViT-L-14-laion2B-s32B-b82K` is 768-dim (~1.7 GB first download). Embeddings are still L2-normalized via `normalize_vector` before storage and query so cosine similarity == dot product, but the dimension is now per-model. imgfind resolves the active model from the `models` DB registry; `imgfind::models::ensure_and_activate_model` auto-registers a clipper-supported model and validates its dim, and the embedder is constructed via `ClipEmbedder::from_model(active_name, false)` (loaded lazily per command). Adding further models is a `clipper`-catalog change.

- **Decode seam (`src/decode.rs`)** — every pixel-decode (indexing, thumbnails, TUI, metadata dimensions, inline display) routes through `decode_image`. RAW files (extensions in `RAW_EXTENSIONS`) are decoded via `rawler` — largest embedded preview first, full demosaic (`RawDevelop`) as fallback; all other formats use the `image` crate. `decode_image` also applies the EXIF `Orientation` tag (0x0112, read via `kamadak-exif`) so RAW previews and oriented JPEGs come out upright; this is fix-forward only (existing cached thumbnails are not regenerated). A separate `decode_full_image` is used by the GUI lightbox for full-screen viewing: for RAW it uses the largest embedded preview when its long edge is ≥ 2000px, else demosaics the sensor for full resolution (thumbnails/embeddings keep using the faster `decode_image`); the lightbox decodes it on a background thread so a slow demosaic never freezes the UI. See `docs/superpowers/specs/2026-06-17-raw-format-support-design.md`, `docs/superpowers/specs/2026-06-17-exif-orientation-design.md`, and `docs/superpowers/specs/2026-06-18-lightbox-fullres-design.md`.

- **Indexing flow (`index_directory` in main.rs)** — walk dir (honoring `Config` ignore regexes), hash each image with `oshash` (content hash for change detection — skip if path+hash already indexed), embed, normalize, insert, then extract EXIF metadata. Backfills missing metadata at the end.

- **Native GUI (`imgfind-gui/`)** — Slint desktop app. **On startup** the GUI restores the last session from the `ui_state` table (search text, sort, filters, selection, scroll position, and the full ordered result id list — thumbnails stream in lazily, no query re-executed); on a fresh DB it browses all images with the configured default sort (name asc). **Grid**: a `Flickable`-based **virtualized infinite scroll** (Approach C) over the full ordered `Vec<RowMeta>` held in memory. A moving window renders only a bounded tile band; a ~100ms sync timer reads `viewport-y`/`cols`, recomputes the window, and requests 300px thumbnails from a background worker (persisted via `get_or_generate_thumbnail`); decoded images live in a bounded LRU (cap 256). The old paged "Load more" model is gone. **Sort selector** beside the filter bar: Name / Size / Type with a direction-toggle (asc/desc); while a semantic query is active, Relevance is also offered (default). Browse re-queries the DB; search re-sorts the matched rows in memory; Relevance restores the original relevance order. Size/Type tie-break on Name; NULL sizes sort last. Single-click a thumbnail opens a right-side **detail panel** (512px thumbnail + metadata + "Search similar" button); the panel shrinks the grid (reflows columns), does not overlay; Escape closes it. Double-click (or "View full") opens the full-screen **lightbox** (prev/next, keyboard nav, Esc to close); the lightbox uses the persisted 2048px cached thumbnail (`LIGHTBOX_SIZE`) via `get_or_generate_thumbnail` instead of decoding full-res live. **Neighbor preload**: opening or navigating the lightbox/detail panel preloads `n` neighbors in an increasing arc from the focus (`preload_arc`), at the surface's display size (2048 for lightbox, 512 for detail); `n = GuiConfig::preload_neighbors` (default 2). "Search similar" runs a vector search from the seed image's embedding, replaces the grid. Right-click opens the original in the OS viewer. **Keyboard navigation** (`imgfind-gui/src/nav.rs` `move_selection` + the grid/lightbox `FocusScope`s in `app.slint`): vim `h/j/k/l` and arrow keys move a highlighted cursor (green border); the cursor tile scrolls into view in the virtual grid; `Enter` opens the detail panel, `Space` opens the lightbox, `Esc` closes the panel. In the lightbox, `h`/`l` join the arrows for prev/next and mirror `selected-index` so closing returns to the last-viewed tile. See `docs/superpowers/specs/2026-06-18-gui-keyboard-navigation-design.md`, `docs/superpowers/specs/2026-06-19-gui-virtualized-scroll-sort-state-design.md`. **Keyboard selection** (`imgfind-gui/src/selection.rs` pure `Selection`/`SelectionMode` state machine): `Shift+V` enters range-select (anchors the cursor; movement materializes the linear contiguous index run, crossing rows); `v` enters free-select (`Space` toggles the cursor tile in/out). Cursor tile shows a green border, selected tiles yellow (green wins if both). While a selection is active, the tag chords (`mm`/`m<color>`) and the `t` modal apply to all selected images; selection persists after apply, `Esc` clears it. An always-visible **statusline** at the bottom shows mode, result count + total size, and (when selecting) selected count + size (e.g. `NORMAL - 42 images - 1.2 GB`, `VISUAL (RANGE) - … | selected 5 - 120 MB`). Selection is grid-only and ephemeral (not persisted to `ui_state`). See `docs/superpowers/specs/2026-06-20-gui-keyboard-selection-modes-design.md`. A **filter bar** beneath the search bar (file-size range slider, file-type chips, GPS tri-state) live-updates results. The filter model (`imgfind::filters::Filters` + `build_filter_clause`) is shared by `Database::browse_all` and filtered vector search. See `docs/superpowers/specs/2026-06-17-filter-bar-design.md`. **Tagging** — free-text tags on images via: a `t`-key modal (space-separated words), a per-image tag editor in the detail panel, and five color **brushes** (red/green/yellow/purple/blue — each a curated set of tags, applied with `m`+color; colors are input shortcuts only and are never stored on images). A backtick-toggled **left rail** holds the five brush editors and an editable **"Most Recent" (`mm`) staging buffer** (click a tag in the Most Recent area to remove it before re-applying). Chord keys (suppressed while typing): `` ` `` toggles the rail; `t` opens the add-tags modal; `m`+`r/g/y/p/b` paints that brush's tags onto the focused image; `mm` re-applies the Most Recent buffer; `f`+`r/g/y/p/b` loads a brush into the tag filter; `ft` toggles tag filtering on/off. All chord keys work in grid, detail panel, and lightbox. Tag **filtering** in the filter pane: tag editor + AND/OR toggle + a slide Toggle to enable/disable; combines with existing filters via the shared `Filters`/`build_filter_clause` seam — no schema migration (uses existing `tags`/`image_tags` tables). Brush definitions, the Most Recent buffer, rail visibility, and tag-filter state persist in `ui_state`. New modules: core `src/colors.rs` (`BrushColor`, `TagBrush`), `Filters` tag fields + `Filters::carry_tag_filter_from`; GUI `imgfind-gui/src/chords.rs` (chord state machine), `imgfind-gui/src/tagset.rs` (tag-list helpers); Slint `imgfind-gui/ui/toggle.slint` (reusable slide-toggle) + `imgfind-gui/ui/tag_editor.slint` (text⇄pills tag editor). See `docs/superpowers/specs/2026-06-20-gui-tag-support-design.md`. The map view is not yet ported. DB is resolved via walk-up/global logic as the CLI; pass `--dir DIR` to target a directory's DB.

- **TUI (`src/tui/`)** — ratatui + `ratatui-image`, gated behind the `tui` feature. `app.rs` holds the event loop (`tokio::select!` over crossterm events, async search results, and async image-decode results via unbounded channels). Vim-style keys: `e` edit search, `h/j/k/l` focus, `H/L` page (9 images/page), `1-9` or `Enter` zoom, scroll/right-click zoom, `q`/`Ctrl-C` quit. Submodules under `app/` (focus, input, search, zoom) and `widget/` (image, center, nine_block grid).

- **Map clustering (`get_images_by_bounds` in database.rs)** — `downsample_by_grid` buckets images into a lat/long grid and samples per cell; `apply_stable_jitter` adds deterministic (hash-based) offsets so co-located markers don't overlap. This is a library function; no frontend currently exposes it (the GUI has no map view yet).

## Conventions

- Rust edition 2024. Errors use `anyhow` (`Context`/`with_context` everywhere).
- Logging via `tracing` (set up in `src/logging.rs`); `RUST_LOG=info|debug` controls verbosity.
- Config lives at `~/.imgfind/config.toml`; view with `imgfind config show`. Ignore patterns are treated as regexes with a substring fallback (`src/config.rs`). The `[gui]` section controls GUI behavior:
  - `preload_neighbors` (default `2`) — neighbors preloaded when the lightbox/detail panel opens.
  - `default_sort` (`"name"` | `"size"` | `"type"`, default `"name"`) — initial sort key for browse-all on startup and after a fresh launch.
  - `default_sort_direction` (`"asc"` | `"desc"`, default `"asc"`) — initial sort direction.
