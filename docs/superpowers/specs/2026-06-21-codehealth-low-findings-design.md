# code-health execution spec — 2026-06-21 low-severity batch

Source: `bughunt.md` triage (2026-06-20). This spec covers **only** the four
findings the user checked `[x] execute`: **B16, B17, B18, B19**. Each is fixed in
its own commit (`fix(<category>): <summary> [B<n>]`) and stripped from
`bughunt.md` in that same commit.

Ranking order (impact = severity × blast-radius): B16 (4) ≈ B17 (3) > B18 (2) ≈ B19 (2).
All are `risk: low` per triage. Per the code-health per-task contract, none
require a pre-written failing characterization test (that gate is `risk: high`),
but each behavioral fix lands with a unit test that would have caught the bug.

## Invariants this feature depends on
- **`Filters` size range is well-ordered**: downstream `build_filter_clause` /
  SQL assume `size_min <= size_max` whenever both are `Some`. B16 hardens the one
  producer (`build_filters`) that could violate it. The new test pins this.
- **Async row streaming**: `turso` returns rows via `rows.next().await?` (an async
  pull, not a sync `Iterator`), so B17 cannot use `.collect::<Result<_,_>>()`;
  the only correctness-preserving win is a capacity hint. See B17 below.
- **Per-path metadata is session-stable**: an indexed image's `image_metadata`
  row does not change while the GUI is open (re-index is a separate process), so
  B19's cache keyed by relative path needs no generation-bump invalidation.

---

## B16 — Size-slider fractions can yield inverted `size_min > size_max`
- Category: api-surface · Impact 4 · Effort S · Risk low
- File: `imgfind-gui/src/main.rs` — `build_filters` (around line 204).
- Problem: `fraction_to_bytes(lo, …, true)` and `(hi, …, false)` are assigned to
  `size_min` / `size_max` with no `lo <= hi` guard. A malformed slider state could
  produce `size_min > size_max`, which `Filters`/SQL assume never happens (silently
  returns an empty/incorrect result set).
- Fix: in `build_filters`, after computing `size_min` and `size_max`, when **both
  are `Some` and `size_min > size_max`, swap them** so the range is always
  well-ordered. (Swap, not drop: a swap preserves the user's intended bounds;
  dropping both would silently widen the filter.)
- Test: unit test on `build_filters` (it is a free function) — feed an inverted
  fraction pair (`lo > hi` mapping to `min > max` bytes) with non-degenerate
  `size_bounds`, assert the returned `Filters` has `size_min <= size_max`. Add a
  companion test that a normal `lo < hi` pair is left unchanged.

## B17 — Redundant `Vec` allocation in search-result collection
- Category: caching · Impact 3 · Effort S · Risk low
- File: `src/database.rs` — three async row-drain loops at ~773–779, ~810–821,
  ~859–867 (`search_similar_images`, `search_similar_images_with_raw_blob`,
  `search_similar_images_meta`).
- Problem: each starts `let mut out = Vec::new();` then pushes one row at a time.
- Correction to the triage's proposed fix: these are **async streams**
  (`while let Some(row) = rows.next().await?`), so `.collect::<Result<Vec<_>,_>>()`
  does **not** apply (no synchronous `Iterator`). The correctness-preserving,
  allocation-reducing change is to **pre-size the Vec with a capacity hint** equal
  to the known upper bound on row count:
  - `search_similar_images` (773): `Vec::with_capacity(k)`.
  - `search_similar_images_with_raw_blob` (810): `Vec::with_capacity(k)`.
  - `search_similar_images_meta` (859): `Vec::with_capacity(limit.min(k))`.
  This avoids the repeated re-grow without changing behavior or ordering.
- Test: none required — behavior is identical (same rows, same order); this is a
  pure allocation optimization. Existing search tests cover the result contents.

## B18 — Malformed GUI `config.toml` silently ignored
- Category: api-surface · Impact 2 · Effort S · Risk low
- File: `imgfind-gui/src/main.rs` — `gui_config` load (around 269–274).
- Problem: `Config::load()` failure is `unwrap_or_else(|e| { tracing::warn!(…);
  GuiConfig::default() })`. A corrupt `config.toml` is only visible if `RUST_LOG`
  is set, so the user silently loses their config with no terminal indication.
- Fix: keep the default-fallback (do **not** abort the GUI on bad config), but on
  the error branch also emit a discoverable **`eprintln!`** to stderr naming the
  config path and the parse error, so a user launching from a terminal sees it
  without `RUST_LOG`. Retain the existing `tracing::warn!`.
- Test: none practical (the branch is an I/O-driven `eprintln!` with no return
  value to assert); the change is a one-line surgical addition. Verified by build.

## B19 — Detail-panel metadata re-fetched from DB on every open
- Category: caching · Impact 2 · Effort S · Risk low
- Files: `imgfind-gui/src/backend.rs` (`metadata`), `imgfind-gui/src/main.rs`
  (`spawn_detail_meta`, ~3289).
- Problem: `spawn_detail_meta` calls `backend.metadata(path)` — a DB round-trip —
  every time the detail panel opens an image, even repeatedly for the same image
  during back/forward navigation. Unlike decoded images (`detail_cache`), there is
  no client-side metadata cache.
- Fix: add a small bounded **process-wide** metadata cache keyed by relative path.
  Because the metadata read runs on a **background thread** (`spawn_detail_meta`),
  the cache must be `Send` — use a `Mutex<LruCache<String, ImageMetadata>>` (NOT
  the thread-local pattern `detail_cache` uses, which is UI-thread-only). New
  module `imgfind-gui/src/meta_cache.rs`:
  - `get(key: &str) -> Option<ImageMetadata>` (clones out, promotes LRU).
  - `insert(key: String, meta: ImageMetadata)`.
  - Capacity ~128 (metadata structs are tiny). Use a `static` `Mutex<LruCache>`
    via `std::sync::OnceLock` / `LazyLock`, matching the crate's existing idioms.
  - `spawn_detail_meta` checks `meta_cache::get` first; on hit, marshals straight
    to the UI thread (no DB call). On miss, fetches via `backend.metadata`, then
    `meta_cache::insert` before painting.
  - No generation-bump invalidation: per the invariant above, an image's metadata
    is session-stable (re-index is a separate process). Document this in the
    module rustdoc.
- Test: unit test on `meta_cache` round-trip (insert then get returns the same
  metadata; get on an absent key returns `None`). Requires `ImageMetadata: Clone`
  — verify; if not already `Clone`, deriving it on the struct is in-scope and
  low-risk (it is a plain data record).

---

## Out of scope (left unchecked in `bughunt.md`)
B9 (ANSI in file logs), B12 (thumbnail writer-panic → `Ok(0)`), B15 (open-external
no UI feedback). Not touched this batch.
