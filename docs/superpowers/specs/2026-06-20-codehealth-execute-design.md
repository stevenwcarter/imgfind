# Code-Health Execute Batch — 2026-06-20

Execution spec for the `bughunt.md` items the user checked `[x] execute`. Source
triage: `bughunt.md` (Last triage 2026-06-20 against `main` @ de6dcd24). One
commit per finding; each fixing commit also strips its finding from `bughunt.md`.
Commit format `fix(<category>): <summary> [B<n>]`. Toolchain for every task:
`cargo build --workspace` / `cargo test --workspace` / `cargo clippy --workspace --all-targets`.

All Rust edits go through the `rust-developer` agent (clippy/fmt-clean, edition
2024). No item here is `risk: high` (the one high-risk finding, B4, was skipped),
so no mandatory RED-first characterization test is required — but each fix should
still land with a test that would have caught the bug where one is feasible.

## Invariants these fixes depend on
- Image paths in the DB are **relative to the DB parent dir**; `relative_to_abs_path` /
  `abs_to_relative_path` (src/lib.rs) convert at every FS boundary. B2 restores
  this invariant in the CLI search path.
- sqlite-vec KNN requires `k >= offset + limit` to fill a page past the first
  window. B8 depends on this.
- Logging is `tracing`; file appender must stay greppable (no ANSI). Several
  observability fixes route failures through `tracing::warn!`.

---

## B1 — Thumbnail decode errors silently dropped with `.ok()` in loader worker
- Category: observability · Impact 16 · Effort S · Risk low
- File: `imgfind-gui/src/loader.rs:59`
- Fix: the `decode_thumb_bytes()` (JPEG→Slint image) result is `.ok()`'d, hiding a
  corrupt-blob failure. Replace with an explicit `match`: on `Err`, `tracing::warn!`
  including the image path/key before returning `None`; on `Ok`, return `Some(img)`.
- Test: unit-test the decode helper returns `None` + (observably) does not panic on
  a deliberately corrupt JPEG byte slice. If the worker closure isn't unit-testable,
  extract the decode+log step into a small named fn and test that.

## B2 — CLI search: relative DB paths passed to `canonicalize()`, resolved against cwd
- Category: correctness · Impact 15 · Effort S · Risk medium
- File: `src/main.rs:765` (search command path handling; also ~814-843)
- Fix: `search_similar_images` returns DB-relative paths. Before any
  `canonicalize()` / prefix-filter / display, convert via
  `relative_to_abs_path(Path::new(path), &db.parent_dir)`. Correct the misleading
  comment that claims the paths are absolute. Apply at both result-handling sites.
- Test: integration test — index a fixture dir whose DB is **not** under the test's
  cwd, run the search code path, assert returned/filtered paths resolve to the real
  files (and that a `--`-style path prefix filter matches correctly). This pins the
  relative-path invariant at the CLI seam.

## B3 — Pervasive `.lock().unwrap()` in GUI event loop (poison-on-panic cascade)
- Category: api-surface · Impact 15 · Effort M · Risk medium
- File: `imgfind-gui/src/main.rs` (~149 sites) + any other imgfind-gui modules
  holding the same locks.
- **Decision (user-selected): adopt `parking_lot::Mutex`** (non-poisoning).
- Fix:
  1. Add `parking_lot` to `imgfind-gui/Cargo.toml`.
  2. Replace the `std::sync::Mutex`/`RwLock` types used for the shared GUI state
     with `parking_lot::Mutex`/`RwLock`.
  3. Replace every `.lock().unwrap()` / `.read().unwrap()` / `.write().unwrap()` on
     those locks with the bare `.lock()` / `.read()` / `.write()` (parking_lot
     guards return directly).
  4. Confirm no `.lock()` result is still being `?`/`.unwrap()`'d, and that no code
     relied on poison semantics (there is none — all sites unwrap).
  5. Keep guard scopes exactly as-is (do not hold a guard across a Slint
     `set_*`/`invoke_*` call — preserve existing tight scoping; this is a known
     deadlock hazard).
- Test: build + clippy clean is the primary gate (mechanical change). No behavior
  test required; verify the app still builds and the existing GUI tests pass.

## B5 — GUI startup `size_bounds()`/`extensions()` failures silently default filter UI with no log
- Category: observability · Impact 12 · Effort S · Risk low
- File: `imgfind-gui/src/main.rs` (startup; approx lines 276/279 and 1740/1743 —
  the real current call sites for `backend.size_bounds()` and `backend.extensions()`)
- Fix: before the `unwrap_or((0,0))` / `unwrap_or_default()` fallback, `map_err` +
  `tracing::warn!` the specific error so a degraded filter bar is diagnosable. Keep
  the fallback behavior (don't crash startup).
- Test: low-risk log-only change; covered by build/clippy. Add a unit test only if a
  seam exists to assert the warn path is taken on `Err`.

## B6 — `thumbnails --size 0` panics in `image.resize()`
- Category: api-surface · Impact 12 · Effort S · Risk low
- File: `src/thumbnail.rs:176`; validation belongs at the CLI boundary
  (`src/main.rs` size parsing / `resolve_thumbnail_sizes`).
- Fix: reject `size == 0` at the CLI parse / `resolve_thumbnail_sizes` boundary with
  a descriptive `anyhow::Error` (e.g. "thumbnail size must be ≥ 1"). Do not let 0
  reach `generate_thumbnail_bytes`.
- Test: unit test that the size validator/`resolve_thumbnail_sizes` returns `Err` for
  `0` and `Ok` for a positive size.

## B7 — No `busy_timeout` on main r2d2 pool; possible stall under concurrent load
- Category: caching · Impact 9 · Effort S · Risk medium
- File: `src/database.rs:60` (pool builder in `Database::new`)
- Fix: add an r2d2 `connection_customizer` (or equivalent on-acquire hook) that sets
  `conn.busy_timeout(Duration::from_secs(5))` on every pooled connection, so a
  contended query fails/limits rather than hanging indefinitely. Match the existing
  busy-timeout already used by the thumbnail batch writer (~line 89) for consistency.
- Test: a contention test is flaky/hard; rely on build + an assertion (if reachable)
  that the customizer is wired. Keep it surgical.

## B8 — Pagination clamp re-caps `k` below `offset+limit` in `search_similar_images_meta`
- Category: correctness · Impact 8 · Effort S · Risk low
- File: `src/database.rs:845`
- Fix: `k = max_k.max(offset+limit).clamp(1, max_k)` wrongly re-caps `k` down to
  `max_k`. Compute `k = (offset + limit).max(1)` so KNN returns enough rows to fill
  a page past `max_k` (treat `max_k` only as the lower-bound default, not an upper
  cap on `k`). Verify the surrounding query still passes `k` and `OFFSET` correctly.
- Test: unit/integration test that requesting a page where `offset + limit > max_k`
  returns a full page (not a short/empty one).

## B10 — Session-state restore failure silently swallowed (`.ok().flatten()`)
- Category: observability · Impact 6 · Effort S · Risk low
- File: `imgfind-gui/src/main.rs:1688`
- Fix: replace `get_ui_state().ok().flatten().unwrap_or_default()` with an explicit
  `match` that `tracing::warn!`s on `Err` before defaulting. First check whether
  `Database::get_ui_state` already logs on its error/mismatch path; if so, log at a
  complementary level (or just the restore-context message) to avoid double-logging
  the same error.
- Test: log-only; build/clippy gate. Add a seam test only if cheap.

## B11 — N+1 query: `image_id` lookup per tag operation
- Category: caching · Impact 6 · Effort M · Risk low
- File: `src/database.rs:534` (`tag_image`/`untag_image`, each calling `image_id_for`
  at ~515); GUI chord handler at `imgfind-gui/src/main.rs:2400`
- Fix: add `batch_tag_images(rel_paths: &[...], tag: &str)` (and `batch_untag_images`)
  that resolves all image ids in one `WHERE path IN (...)` query and inserts/deletes
  the tag rows in a single transaction. Wire the GUI multi-select chord apply path to
  the batch fns instead of looping the single-image fns. Keep the single-image fns for
  the single-tile path.
- Test: DB test that batch-tagging N images attaches the tag to all N and is
  equivalent to looping the single fn (same final state), in one transaction.

## B13 — Thumbnail requests not deduped within a loader tick (fast-scroll herd)
- Category: caching · Impact 6 · Effort M · Risk low
- File: `imgfind-gui/src/loader.rs:98` (in_flight set); tick at `main.rs:2771`
- Fix: within a single loader tick, diff the visible/needed paths against
  `cache ∪ in_flight` and enqueue each newly-needed path **once**, marking it
  in_flight at enqueue time so a later tick in the same fling doesn't re-enqueue it
  before the worker responds. Preserve the cross-tick dedup already present.
- Test: unit-test the per-tick selection helper: given a needed-set, a cache set, and
  an in_flight set, it returns only the genuinely-new paths and updates in_flight.
  Extract the selection logic into a pure fn if needed to test it.

## B14 — `rehydrate_rows` does N+1 queries on session restore
- Category: caching · Impact 4 · Effort S · Risk low
- File: `src/database.rs:1416`; caller `imgfind-gui/src/main.rs:1896`
- Fix: replace the per-id query loop with a single `WHERE id IN (...)` query joined
  to metadata (mirror `browse_all` / `search_similar_images_meta`); build a
  `HashMap<id, row>` and reorder to preserve the input id order exactly.
- Test: DB test that `rehydrate_rows([id3, id1, id2])` returns rows in input order
  with metadata populated, equivalent to the previous per-id behavior.

---

## Execution order
Process in `bughunt.md` order (impact desc): B1, B2, B3, B5, B6, B7, B8, B10, B11,
B13, B14. Milestone full-suite run after B6 (5th item) and at end of batch.
B3 is the largest/riskiest mechanical change — run build+clippy+full tests
immediately after it regardless of milestone position.
