# bughunt.md — code-health audit findings

Last triage: 2026-06-20 against `main` @ de6dcd24. Toolchain: cargo build --workspace / cargo test --workspace / cargo clippy --workspace --all-targets.

> **For future sessions reading this file:** when you fix an item listed
> here, strip it from this file in the same commit that fixes it. The list
> is intended to reflect open issues only; resolved items shouldn't linger.
> This keeps the file's signal-to-noise high for the next audit pass.

## How to use this file
- Check `[x] execute` on items to fix this batch.
- Check `[x] skip` on items to never re-flag (the skill records them in user memory).
- Items left unchecked stay in bughunt.md for the next run.
- Ranking is impact = severity × blast-radius (effort is shown separately, never folded into the rank).
- When ready, run `/code-health --execute`.

## Critical

_(none)_

## High

### B2. CLI search: relative DB paths passed to `canonicalize()`, resolved against cwd: `search command path handling` (src/main.rs:765)
- Category: correctness
- Impact: 15 (severity 5 × blast-radius 3)
- Effort: S
- Risk: medium
- Evidence: `search_similar_images` returns paths relative to the DB parent dir, but the search command treats them as absolute and calls `canonicalize()` (~769), which resolves relative to cwd, not the DB parent. When the DB is not under cwd this wrongly filters/displays results and the path-prefix filter misbehaves.
- Blast radius: src/main.rs:757-790, src/main.rs:814-843
- Proposed fix: Convert with `relative_to_abs_path(Path::new(path), &db.parent_dir)` before filtering/canonicalizing; fix the misleading comment.
- [x] execute   [ ] skip

### B3. Pervasive `.lock().unwrap()` in GUI event loop (poison-on-panic cascade): `Mutex locks across main.rs` (imgfind-gui/src/main.rs:121)
- Category: api-surface
- Impact: 15 (severity 3 × blast-radius 5)
- Effort: M
- Risk: medium
- Evidence: ~149 `.lock().unwrap()` with no poison recovery; a panic in any thread holding a Mutex poisons it and every later caller panics with an opaque error, crashing the app and masking the original panic.
- Blast radius: imgfind-gui/src/main.rs:121, imgfind-gui/src/main.rs:461, imgfind-gui/src/main.rs:485-487, imgfind-gui/src/main.rs:737
- Proposed fix: Adopt `parking_lot::Mutex` (non-poisoning) or a small lock helper with `unwrap_or_else(|p| p.into_inner())`; add `catch_unwind`+log in spawned threads.
- [x] execute   [ ] skip

> **decision-needed (architectural):** B3 is a cross-cutting Mutex-strategy
> change touching ~149 call sites — pick the approach (parking_lot vs. a
> poison-recovering helper) before executing; not a drop-in surgical patch.

### B5. GUI startup `size_bounds()`/`extensions()` failures silently default the filter UI with no log: `size_bounds/extensions startup` (imgfind-gui/src/main.rs:1740)
- Category: observability
- Impact: 12 (severity 4 × blast-radius 3)
- Effort: S
- Risk: low
- Evidence: `backend.size_bounds().unwrap_or((0,0))` and `extensions().unwrap_or_default()` at startup: a DB error silently yields a zero size range / empty type chips (filter bar wrong) with no log entry, so the degraded filter UI is undiagnosable. (Merged: the api-surface and observability lenses flagged the same root cause at approximate lines 276/279 and 1740/1743.)
- Blast radius: imgfind-gui/src/main.rs:276, imgfind-gui/src/main.rs:279, imgfind-gui/src/main.rs:1740, imgfind-gui/src/main.rs:1743
- Proposed fix: `map_err` + `tracing::warn!` the specific error before the `unwrap_or` fallback; consider a user-facing error if these core queries fail.
- [x] execute   [ ] skip

### B6. `thumbnails --size 0` panics in `image.resize()`: `generate_thumbnail_bytes` (src/thumbnail.rs:176)
- Category: api-surface
- Impact: 12 (severity 4 × blast-radius 3)
- Effort: S
- Risk: low
- Evidence: CLI accepts `--size 0` unvalidated; `resolve_thumbnail_sizes` passes it to `generate_thumbnail_bytes` -> `image.resize(0,0,..)` which panics. Reachable from `imgfind thumbnails --size 0`.
- Blast radius: src/main.rs:283, src/main.rs:1026, src/thumbnail.rs:176
- Proposed fix: Reject `size == 0` in the CLI size parser / `resolve_thumbnail_sizes` with a descriptive `anyhow::Error`.
- [x] execute   [ ] skip

## Medium

### B7. No `busy_timeout` on main r2d2 pool; possible stall under concurrent load: `Database::new pool` (src/database.rs:60)
- Category: caching
- Impact: 9 (severity 3 × blast-radius 3)
- Effort: S
- Risk: medium
- Evidence: r2d2 pool `max_size = parallelism().min(32)`. The thumbnail worker holds one connection in a loop; concurrent grid/detail/metadata queries can exhaust the pool. No `busy_timeout` on the main pool (only the thumbnail batch writer at 89), so a contended query can hang indefinitely.
- Blast radius: src/database.rs:64, imgfind-gui/src/loader.rs:36, imgfind-gui/src/preload.rs
- Proposed fix: Add a `connection_customizer` to the pool builder in `Database::new` setting `conn.busy_timeout(2-5s)`.
- [x] execute   [ ] skip

### B8. Pagination clamp re-caps `k` below `offset+limit` in `search_similar_images_meta`: `search_similar_images_meta` (src/database.rs:845)
- Category: correctness
- Impact: 8 (severity 4 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `k = max_k.max(offset+limit).clamp(1,max_k)` re-caps `k` to `max_k` even when `offset+limit` exceeds `max_k`, so a page past `max_k` can't return a full window (sqlite-vec `MATCH+k+OFFSET` needs `k >= offset+limit`).
- Blast radius: src/database.rs:845-856
- Proposed fix: Compute `k = (offset+limit).max(1)` without re-capping to `max_k` (or treat `max_k` purely as a floor/ceiling guard). Add a pagination-beyond-`max_k` test.
- [x] execute   [ ] skip

### B9. ANSI escape codes written into file logs (`with_ansi(true)`): `logging init` (src/logging.rs:29)
- Category: observability
- Impact: 6 (severity 3 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: The non-blocking file appender is configured `with_ansi(true)`, so `log.txt` gets ANSI escapes that corrupt grep/parsers.
- Blast radius: src/logging.rs:29
- Proposed fix: Set `.with_ansi(false)` on the file writer (keep color only for a TTY terminal layer if desired).
- [ ] execute   [ ] skip

### B10. Session-state restore failure silently swallowed (`.ok().flatten()`): `get_ui_state restore` (imgfind-gui/src/main.rs:1688)
- Category: observability
- Impact: 6 (severity 3 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `get_ui_state().ok().flatten().unwrap_or_default()` suppresses any restore error so the user loses their prior session with no log explaining why.
- Blast radius: imgfind-gui/src/main.rs:1688
- Proposed fix: Replace with an explicit `match` that `tracing::warn!`s on `Err` before defaulting. Verify whether `Database::get_ui_state` already logs to avoid double-logging.
- [x] execute   [ ] skip

### B11. N+1 query: `image_id` lookup per tag operation: `tag_image/untag_image` (src/database.rs:534)
- Category: caching
- Impact: 6 (severity 3 × blast-radius 2)
- Effort: M
- Risk: low
- Evidence: `tag_image` and `untag_image` both call `image_id_for` (515), a separate query. Tagging a GUI multi-selection fires one id-lookup query per image instead of batching.
- Blast radius: src/database.rs:534, src/database.rs:546, imgfind-gui/src/main.rs:2400
- Proposed fix: Add `batch_tag_images(rel_paths, &tag)` fetching all ids in one `WHERE path IN (...)` query then insert in a transaction; same for untag. Wire into the GUI chord handler.
- [x] execute   [ ] skip

### B12. Thumbnail writer-thread panic reported to CLI as `Ok(0)`: `generate_missing_thumbnails_batch` (src/thumbnail.rs:165)
- Category: api-surface
- Impact: 6 (severity 3 × blast-radius 2)
- Effort: M
- Risk: low
- Evidence: The writer thread panics if `Database::new()`/`pool.get()` fail (explicit `panic!` at 75-77); `join()` at 164 logs but the function still returns `Ok(0)`, so the CLI reports success having generated zero thumbnails.
- Blast radius: src/main.rs:284, src/thumbnail.rs:79, src/thumbnail.rs:164
- Proposed fix: Propagate a writer-thread panic/join error as `Err`, or make the writer return a `Result` communicated via a shared channel.
- [ ] execute   [ ] skip

### B13. Thumbnail requests not deduped within a loader tick (fast-scroll herd): `loader tick/in_flight` (imgfind-gui/src/loader.rs:98)
- Category: caching
- Impact: 6 (severity 2 × blast-radius 3)
- Effort: M
- Risk: low
- Evidence: `in_flight` HashSet dedups across ticks, but on a fast fling the 100ms tick can request the same uncached tiles repeatedly before responses arrive, queueing redundant decode requests to the worker.
- Blast radius: imgfind-gui/src/loader.rs:98, imgfind-gui/src/main.rs:2771
- Proposed fix: Within each tick, diff visible paths against cache+`in_flight` and send only newly-needed paths once.
- [x] execute   [ ] skip

## Low

### B14. rehydrate_rows does N+1 queries on session restore: `rehydrate_rows` (src/database.rs:1416)
- Category: caching
- Impact: 4 (severity 2 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `rehydrate_rows` (session restore, 80-100 ids) fetches rows by id in a loop = 100+ queries; `browse_all` and `search_similar_images_meta` JOIN in one pass but rehydrate doesn't.
- Blast radius: src/database.rs:1416, imgfind-gui/src/main.rs:1896
- Proposed fix: Replace the per-id loop with one `WHERE id IN (...)` query joined to metadata; build a HashMap and reorder.
- [x] execute   [ ] skip

### B15. Open-in-OS-viewer failure only logged, no UI feedback: `on_tile_open_external` (imgfind-gui/src/main.rs:744)
- Category: api-surface
- Impact: 4 (severity 2 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `open::that(&abs)` errors on right-click-open are caught and `tracing::warn!`'d; the GUI shows nothing, so the user gets no indication the open failed.
- Blast radius: imgfind-gui/src/main.rs:744
- Proposed fix: On `Err`, surface a transient UI error (toast/status field).
- [ ] execute   [ ] skip

### B16. Size-slider fractions can yield inverted `size_min > size_max`: `build_filters/fraction_to_bytes` (imgfind-gui/src/main.rs:203)
- Category: api-surface
- Impact: 4 (severity 2 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `fraction_to_bytes(lo,..)` and `(hi,..)` at 203-204 aren't checked for `lo <= hi`; a malformed slider state could produce `size_min > size_max`, which the `Filters`/SQL assume never happens.
- Blast radius: imgfind-gui/src/main.rs:203
- Proposed fix: Clamp/swap so `size_min <= size_max` in `build_filters` (or drop both if inverted); add a test.
- [ ] execute   [ ] skip

### B17. Redundant Vec allocation in search result collection: `search_similar_images` (src/database.rs:771)
- Category: caching
- Impact: 3 (severity 1 × blast-radius 3)
- Effort: S
- Risk: low
- Evidence: `search_similar_images` allocates an empty Vec then loops to push from `query_map` instead of `collect()`. Same at 818 and 881. Idiomatic but slightly less efficient than `collect` with a capacity hint.
- Blast radius: src/database.rs:771, src/database.rs:818, src/database.rs:881
- Proposed fix: Replace the `Vec::new()` loop with `.collect::<Result<Vec<_>,_>>()` at 771-774, 818-821, 881-884.
- [ ] execute   [ ] skip

### B18. Malformed GUI `config.toml` silently ignored, defaults applied: `GuiConfig load` (imgfind-gui/src/main.rs:267)
- Category: api-surface
- Impact: 2 (severity 1 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `Config::load()` is `unwrap_or_else -> GuiConfig::default()` with only a `tracing::warn!`; a corrupt `config.toml` is silently ignored and only visible if `RUST_LOG` is set.
- Blast radius: imgfind-gui/src/main.rs:267
- Proposed fix: Keep the default-fallback but make the warning more discoverable (stderr line on parse error). Small surgical change.
- [ ] execute   [ ] skip

### B19. Detail-panel metadata re-fetched from DB on every open: `backend.metadata` (imgfind-gui/src/backend.rs:174)
- Category: caching
- Impact: 2 (severity 1 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `backend.metadata()` queries the DB every time the detail panel opens an image; no client-side cache across opens (unlike `detail_cache` for decoded images).
- Blast radius: imgfind-gui/src/backend.rs:174, imgfind-gui/src/detail.rs
- Proposed fix: Add a small metadata cache keyed by relative path, invalidated on generation bump.
- [ ] execute   [ ] skip

## Skip (do not re-flag in future runs)
- `SearchState.results` unbounded `Vec<RowMeta>` at imgfind-gui/src/state.rs:19 — search results are 100 by default and only "relevance" is in-memory sorted; O(100) is fine.
