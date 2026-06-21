# Code-Health Execute Batch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the 10 `bughunt.md` findings the user selected for execution (B1, B2, B3, B5, B6, B7, B8, B10, B11, B13, B14), one commit per finding, each commit also stripping its finding from `bughunt.md`.

**Architecture:** Surgical fixes + targeted refactors across the `imgfind` core crate (CLI/DB/thumbnail/logging) and the `imgfind-gui` Slint crate (loader, startup, locks). No public-API rewrites. B3 is a mechanical dependency swap to `parking_lot` (user-chosen).

**Tech Stack:** Rust edition 2024, rusqlite + r2d2 (sqlite-vec), Slint 1.x, tracing, anyhow, parking_lot (added in B3).

## Global Constraints
- Edition 2024; all Rust edits go through the `rust-developer` agent and must be `cargo clippy --workspace --all-targets`-clean and `cargo fmt --all`-clean.
- Errors use `anyhow` with `Context`/`with_context`. Diagnostics use `tracing`, never `println!`/`eprintln!`.
- Toolchain for every task: `cargo build --workspace` / `cargo test --workspace` / `cargo clippy --workspace --all-targets`.
- Commit format: `fix(<category>): <summary> [B<n>]`. Each fixing commit ALSO removes that finding's block from `bughunt.md` (strip-on-fix). Stage both the code and `bughunt.md` in the same commit.
- Image paths in the DB are **relative to `Database::parent_dir`**; convert with `imgfind::relative_to_abs_path` / `abs_to_relative_path` at every FS boundary.
- Preserve Slint guard scoping: never hold a lock guard across a Slint `set_*`/`invoke_*` call.
- No finding in this batch is `risk: high`, so no mandatory RED-first characterization commit is required. Still add a regression test with each fix where a seam exists.

---

### Task B1: Log thumbnail decode failures in the loader worker

**Files:**
- Modify: `imgfind-gui/src/loader.rs:59` (the `.ok()` on the JPEG→Slint decode)

**Interfaces:**
- Consumes: existing `crate::image_util` decode helper (currently `…().ok()`).
- Produces: no signature change; behavior is now logged.

- [ ] **Step 1:** Read `imgfind-gui/src/loader.rs` around the worker loop (lines ~40–70). Identify the exact decode call whose result is `.ok()`'d at line 59 and the variable holding the path/key being decoded.
- [ ] **Step 2:** Replace the `.ok()` with an explicit match:

```rust
match crate::image_util::/* existing decode fn */(bytes) {
    Ok(img) => Some(img),
    Err(e) => {
        tracing::warn!(path = %/* the key/path var */, "thumbnail decode failed: {e:#}");
        None
    }
}
```

Use the real function name and path variable found in Step 1. Keep the surrounding `Some`/`None` flow identical.
- [ ] **Step 3:** If the decode+log is inside a closure that isn't unit-testable, extract it into a small `fn decode_or_warn(bytes: &[u8], key: &str) -> Option<slint::Image>` and call that. Add a unit test that `decode_or_warn(&[0xFF, 0xD8, 0x00], "k")` returns `None` and does not panic. If extraction isn't clean, skip the test (log-only change) and rely on build/clippy.
- [ ] **Step 4:** Run `cargo build --workspace && cargo clippy --workspace --all-targets`. Expected: clean.
- [ ] **Step 5:** Strip the B1 block from `bughunt.md`. Commit: `fix(observability): log thumbnail decode failures in loader worker [B1]`.

---

### Task B2: CLI search converts DB-relative paths to absolute before filtering

**Files:**
- Modify: `src/main.rs:760-790` (the `filtered_results` closure) and the `short`/standard print loops at `src/main.rs:812-843` if they display paths.
- Test: `tests/` (integration) — new test asserting search filtering works when the DB is not under cwd.

**Interfaces:**
- Consumes: `imgfind::relative_to_abs_path(path: &Path, parent_dir: &Path) -> PathBuf` and `db.parent_dir` (the `Database`'s base dir). Verify the exact accessor name (`db.parent_dir` field or a getter).
- Produces: filtering now compares true absolute paths.

- [ ] **Step 1:** Confirm the bug: `SearchEngine::search` → `Database::search_similar_images` returns `i.path` (DB-relative; see `src/database.rs:765` binding `rel_path`). So `src/main.rs:766` `path_buf.to_path_buf()` keeps it relative and `.canonicalize()` (line 769) resolves against cwd — wrong when the DB isn't under cwd.
- [ ] **Step 2 (write the failing test):** Add an integration test in `tests/` that: creates a temp dir `D` with a couple of small image fixtures, indexes it so the DB lives under `D/.imgfind`, then drives the search path (or a thin extracted helper) **from a different cwd**, and asserts the returned/filtered results include the fixture under `D` (non-recursive: parent == D). If the `cmd_search`-style fn isn't directly callable, extract the path-filtering logic into a pure helper `fn filter_results(results, parent_dir, current_dir, all, recursive, limit) -> Vec<(String,f32)>` in `src/main.rs` (or a lib module) and test that helper directly with relative input paths + a `parent_dir` ≠ `current_dir`.
- [ ] **Step 3:** Run the new test; expected FAIL (relative path canonicalizes against cwd, so the filter drops the result or matches the wrong dir).
- [ ] **Step 4 (fix):** In the closure, replace lines 765-766:

```rust
// paths from the DB are relative to the DB parent dir — make them absolute
let abs_path = imgfind::relative_to_abs_path(std::path::Path::new(path), &db.parent_dir);
```

Remove the misleading "already absolute" comment. Keep the subsequent `.canonicalize().unwrap_or(abs_path)` and the `all`/`recursive`/parent checks. If the standard/short print loops should show absolute paths too, convert there as well (decide based on prior behavior — minimally, fix the filter; only change display if it was already intended to be absolute). Resolve any borrow of `db` (it is borrowed by `SearchEngine::new(db)`; read `db.parent_dir` before/after as the borrow checker requires — `parent_dir` is a `PathBuf`, clone if needed).
- [ ] **Step 5:** Run the test; expected PASS. Run `cargo test --workspace`, `cargo clippy --workspace --all-targets`. Expected clean.
- [ ] **Step 6:** Strip B2 from `bughunt.md`. Commit: `fix(correctness): convert DB-relative search paths to absolute before filtering [B2]`.

---

### Task B3: Adopt parking_lot::Mutex in imgfind-gui (drop poison cascade)

**Files:**
- Modify: `imgfind-gui/Cargo.toml` (add `parking_lot`)
- Modify: `imgfind-gui/src/*.rs` — every `std::sync::Mutex`/`RwLock` used for shared GUI state and all `.lock().unwrap()` / `.read().unwrap()` / `.write().unwrap()` call sites (~149).

**Interfaces:**
- Consumes: nothing new beyond the `parking_lot` crate.
- Produces: lock guards obtained via bare `.lock()`/`.read()`/`.write()`.

- [ ] **Step 1:** Add the dependency. Check `Cargo.lock`/workspace for an existing `parking_lot` version to match; otherwise `parking_lot = "0.12"`. Add under `imgfind-gui/Cargo.toml [dependencies]`.
- [ ] **Step 2:** Enumerate the lock types: `rg 'std::sync::(Mutex|RwLock)|Mutex::new|RwLock::new' imgfind-gui/src`. Change those type usages and constructors to `parking_lot::Mutex` / `parking_lot::RwLock` (constructors are identical: `Mutex::new(x)`). Update imports.
- [ ] **Step 3:** Replace lock acquisition: `rg '\.lock\(\)\.unwrap\(\)|\.read\(\)\.unwrap\(\)|\.write\(\)\.unwrap\(\)' imgfind-gui/src`. Change each to the bare `.lock()` / `.read()` / `.write()` (parking_lot guards return directly, no `Result`). Watch for any `.lock().expect(...)` or `if let Ok(g) = x.lock()` variants and convert them too.
- [ ] **Step 4:** Verify no remaining `.lock()`/`.read()`/`.write()` is being `?`-propagated or `.unwrap()`'d on these parking_lot locks, and that guard scopes are unchanged (still dropped before any Slint `set_*`/`invoke_*` — do NOT move any guard into a wider scope).
- [ ] **Step 5:** Run `cargo build --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt --all`. Expected: clean, no `unused` `Result` warnings, no leftover `.unwrap()` on locks.
- [ ] **Step 6:** Run `cargo test --workspace` (full suite — this is the largest change; run regardless of milestone). Expected: green.
- [ ] **Step 7:** Strip B3 from `bughunt.md` (both the finding block and its `decision-needed` blockquote). Commit: `fix(api-surface): use parking_lot::Mutex in GUI to drop poison-on-panic cascade [B3]`.

---

### Task B5: Log GUI startup size_bounds()/extensions() failures before fallback

**Files:**
- Modify: `imgfind-gui/src/main.rs` — the startup calls `backend.size_bounds().unwrap_or((0,0))` and `backend.extensions().unwrap_or_default()` (grep for them; lenses reported approx lines 276/279 and 1740/1743).

**Interfaces:**
- Consumes: existing `backend.size_bounds() -> Result<(u64,u64)>` and `backend.extensions() -> Result<Vec<...>>` (confirm exact signatures).
- Produces: same fallback values, now logged on error.

- [ ] **Step 1:** `rg 'size_bounds\(\)|extensions\(\)' imgfind-gui/src/main.rs` to find the exact startup sites and confirm the fallbacks.
- [ ] **Step 2:** Replace each silent fallback with a logged one, e.g.:

```rust
let size_bounds = backend.size_bounds().unwrap_or_else(|e| {
    tracing::warn!("startup: size_bounds query failed, filter slider may be incomplete: {e:#}");
    (0, 0)
});
let extensions = backend.extensions().unwrap_or_else(|e| {
    tracing::warn!("startup: extensions query failed, type chips may be incomplete: {e:#}");
    Vec::new()
});
```

Match the real fallback types (e.g. `unwrap_or_default()` → `unwrap_or_else(|e| { warn; Default::default() })`).
- [ ] **Step 3:** Run `cargo build --workspace && cargo clippy --workspace --all-targets`. Expected: clean.
- [ ] **Step 4:** Strip B5 from `bughunt.md`. Commit: `fix(observability): log GUI startup filter-query failures before defaulting [B5]`.

---

### Task B6: Reject `thumbnails --size 0` at the CLI boundary

**Files:**
- Modify: `src/main.rs` — the thumbnail size CLI parsing / `resolve_thumbnail_sizes` (grep; lens reported ~283, ~1026).
- Test: unit test for the size validator / `resolve_thumbnail_sizes`.

**Interfaces:**
- Consumes: existing size-parsing path that feeds `generate_missing_thumbnails_batch(size)`.
- Produces: `Err` for `size == 0` before reaching `image.resize`.

- [ ] **Step 1:** `rg 'resolve_thumbnail_sizes|fn .*thumbnail.*size|--size' src/main.rs` to find where `--size`/`--gui-sizes` are parsed into the size list.
- [ ] **Step 2 (write failing test):** Add a unit test asserting the validator/`resolve_thumbnail_sizes` returns `Err` when given `0` (e.g. `resolve_thumbnail_sizes(&[0], false)` is `Err`) and `Ok` for `[300]`. If sizes come straight from clap, add a clap `value_parser` validator fn `parse_thumbnail_size(s: &str) -> Result<u32, String>` and test it rejects `"0"`.
- [ ] **Step 3:** Run the test; expected FAIL (currently 0 passes through and would panic downstream).
- [ ] **Step 4 (fix):** Add the zero-rejection at the parse/resolve boundary with a clear `anyhow`/clap error ("thumbnail size must be ≥ 1"). Ensure both `--size 0` and a `--size 0` mixed with valid sizes are rejected.
- [ ] **Step 5:** Run the test; expected PASS. `cargo test --workspace`, `cargo clippy --workspace --all-targets`. Clean.
- [ ] **Step 6:** Strip B6 from `bughunt.md`. Commit: `fix(api-surface): reject thumbnail size 0 at CLI boundary [B6]`.

**MILESTONE (after B6 = 5th item): run full `cargo test --workspace`. On red, bisect within batch, revert offender, surface diagnosis. On green, continue.**

---

### Task B7: Set busy_timeout on every pooled connection in Database::new

**Files:**
- Modify: `src/database.rs:60` (the `r2d2::Pool` builder in `Database::new`).

**Interfaces:**
- Consumes: existing `r2d2_sqlite::SqliteConnectionManager` + `r2d2::Pool::builder()`.
- Produces: pooled connections carry a 5s busy_timeout.

- [ ] **Step 1:** Read `src/database.rs` around `Database::new` (lines ~40–95). Note how the pool is built and how the thumbnail batch writer sets its busy_timeout (~line 89) — mirror that duration.
- [ ] **Step 2 (fix):** Add a `connection_customizer` to the pool builder so every connection runs `conn.busy_timeout(std::time::Duration::from_secs(5))` on acquire. Implement the `r2d2::CustomizeConnection<Connection, rusqlite::Error>` trait (a small unit struct with an `on_acquire` that calls `busy_timeout`) and pass it via `.connection_customizer(Box::new(BusyTimeout))`. Use the same timeout value as the existing writer for consistency.
- [ ] **Step 3:** Run `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets`. Expected: clean (existing DB tests still pass — the customizer is transparent).
- [ ] **Step 4:** Strip B7 from `bughunt.md`. Commit: `fix(caching): set busy_timeout on pooled DB connections [B7]`.

---

### Task B8: Fix pagination clamp in search_similar_images_meta

**Files:**
- Modify: `src/database.rs:845`
- Test: integration/DB test for pagination past `max_k`.

**Interfaces:**
- Consumes: `search_similar_images_meta(embedding, limit, offset, distance_threshold, max_k, filters)`.
- Produces: `k` large enough to fill a page past `max_k`.

- [ ] **Step 1 (write failing test):** Add a DB test that inserts (e.g.) 60 vectors, then calls `search_similar_images_meta` with `max_k = 40`, `offset = 35`, `limit = 20` (so `offset+limit = 55 > max_k`). Assert the returned page has the expected number of rows (a full or correctly-sized window), not a short/empty one caused by `k` being capped at 40.
- [ ] **Step 2:** Run the test; expected FAIL (page is short because `k` is clamped to `max_k = 40 < 55`).
- [ ] **Step 3 (fix):** Change line 845 from `let k = max_k.max(offset + limit).clamp(1, max_k);` to `let k = (offset + limit).max(1).max(max_k);` — i.e. `k` is at least `offset+limit` and at least `max_k`, with no upper re-cap. (Keep `max_k` as a floor for the small-page case.) Update the adjacent comment to describe the corrected intent.
- [ ] **Step 4:** Run the test; expected PASS. `cargo test --workspace`, `cargo clippy --workspace --all-targets`. Clean.
- [ ] **Step 5:** Strip B8 from `bughunt.md`. Commit: `fix(correctness): stop re-capping k below offset+limit in paginated vector search [B8]`.

---

### Task B10: Log session-state restore failures instead of swallowing

**Files:**
- Modify: `imgfind-gui/src/main.rs:1688` (`get_ui_state().ok().flatten().unwrap_or_default()`).

**Interfaces:**
- Consumes: `backend.get_ui_state() -> Result<Option<UiState>>` (confirm exact type).
- Produces: same default-on-failure behavior, now logged.

- [ ] **Step 1:** Check whether `Database::get_ui_state` already `tracing::warn!`s on its error/deser-mismatch path (`rg 'fn get_ui_state' src` then read it). Avoid double-logging the identical error.
- [ ] **Step 2 (fix):** Replace the `.ok().flatten().unwrap_or_default()` with an explicit match:

```rust
let mut st = match backend.get_ui_state() {
    Ok(Some(s)) => s,
    Ok(None) => UiState::default(),
    Err(e) => {
        tracing::warn!("session restore failed, starting with defaults: {e:#}");
        UiState::default()
    }
};
```

Match the real binding name/mutability used downstream.
- [ ] **Step 3:** Run `cargo build --workspace && cargo clippy --workspace --all-targets`. Expected: clean.
- [ ] **Step 4:** Strip B10 from `bughunt.md`. Commit: `fix(observability): log session-state restore failures before defaulting [B10]`.

---

### Task B11: Batch image-id resolution for multi-image tagging

**Files:**
- Modify: `src/database.rs` near `tag_image`/`untag_image` (~515–546) — add `batch_tag_images` / `batch_untag_images`.
- Modify: `imgfind-gui/src/main.rs:2400` (GUI multi-select chord apply) to call the batch fns.
- Test: DB test for batch tag/untag.

**Interfaces:**
- Consumes: existing `image_id_for(rel_path) -> Result<Option<i64>>`, `tag_image`, `untag_image`.
- Produces:
  - `pub fn batch_tag_images(&self, rel_paths: &[&str], tag: &str) -> Result<()>`
  - `pub fn batch_untag_images(&self, rel_paths: &[&str], tag: &str) -> Result<()>`
  (mirror the single-image semantics: create the tag row if needed, insert/delete `image_tags` rows; idempotent.)

- [ ] **Step 1:** Read `tag_image`/`untag_image`/`image_id_for` and the `tags`/`image_tags` schema usage to mirror exact SQL (tag upsert + `image_tags` insert/delete) and the relative-path lookup.
- [ ] **Step 2 (write failing test):** DB test: index/insert 3 images, `batch_tag_images(&[p1,p2,p3], "beach")`, assert all 3 carry the tag (query `image_tags`), and `batch_untag_images(&[p1,p3], "beach")` leaves only p2 tagged. Assert it equals looping the single-image fns.
- [ ] **Step 3:** Run; expected FAIL (fns don't exist).
- [ ] **Step 4 (implement):** Add both fns: resolve all ids in one `SELECT id, path FROM images WHERE path IN (...)` (build the placeholder list, `params_from_iter`), upsert the tag once, then insert/delete `image_tags` rows inside a single transaction. Preserve behavior for missing paths (skip silently or log — match `tag_image`'s current handling). Wire the GUI chord apply path at `imgfind-gui/src/main.rs:2400` to call the batch fn for a multi-selection instead of looping `tag_image`. Keep single-tile path on the single fn (or route through batch with a 1-element slice — implementer's call, keep it DRY).
- [ ] **Step 5:** Run the test; expected PASS. `cargo test --workspace`, `cargo clippy --workspace --all-targets`. Clean.
- [ ] **Step 6:** Strip B11 from `bughunt.md`. Commit: `fix(caching): batch image-id resolution for multi-image tagging [B11]`.

---

### Task B13: Dedup thumbnail requests within a single loader tick

**Files:**
- Modify: `imgfind-gui/src/loader.rs:98` (the `in_flight` set + per-tick request build) and the tick driver at `imgfind-gui/src/main.rs:2771`.
- Test: unit test for the per-tick selection helper.

**Interfaces:**
- Consumes: the cache set, `in_flight` set, and the needed/visible path list for a tick.
- Produces: a pure helper, e.g. `fn select_to_request(needed: &[Key], cache: &LruCache<...>, in_flight: &HashSet<Key>) -> Vec<Key>` returning only genuinely-new keys; caller marks them in_flight at enqueue time.

- [ ] **Step 1:** Read the tick path (loader.rs ~90–110 and main.rs ~2771) to see how visible tiles are turned into worker requests and where `in_flight` is updated.
- [ ] **Step 2 (write failing test or characterize):** Extract the selection logic into a pure `select_to_request` helper. Unit-test: given `needed = [a,b,c]`, `cache` containing `a`, `in_flight` containing `b`, it returns `[c]` only. (If a fast fling currently re-enqueues `b`/`a`, the pre-extraction behavior would include them — assert the helper excludes both.)
- [ ] **Step 3:** Run; expected FAIL until the helper exists / dedup is correct.
- [ ] **Step 4 (fix):** Implement `select_to_request` to diff `needed` against `cache ∪ in_flight`, returning each new key once (dedup within the tick too, in case `needed` repeats). In the tick, mark returned keys as in_flight **at enqueue time** so a later tick in the same fling won't re-enqueue them before the worker responds.
- [ ] **Step 5:** Run the test; expected PASS. `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets`. Clean.
- [ ] **Step 6:** Strip B13 from `bughunt.md`. Commit: `fix(caching): dedup thumbnail requests within a loader tick [B13]`.

---

### Task B14: Single-query rehydrate_rows on session restore

**Files:**
- Modify: `src/database.rs:1416` (`rehydrate_rows`).
- Test: DB test for order-preserving batch rehydrate.

**Interfaces:**
- Consumes: same `rehydrate_rows(ids: &[i64]) -> Result<Vec<RowMeta>>` signature (confirm exact name/types).
- Produces: identical output (rows in input id order, metadata populated), one query instead of N.

- [ ] **Step 1:** Read `rehydrate_rows` and a reference impl that already JOINs metadata in one pass (`browse_all` / `search_similar_images_meta`) to mirror the SELECT + metadata LEFT JOIN.
- [ ] **Step 2 (write failing or characterization test):** DB test: insert 3 images (+metadata), call `rehydrate_rows(&[id3, id1, id2])`, assert returned rows are in `[id3, id1, id2]` order with metadata fields populated. (This characterizes current behavior; it should pass before and after — the change is internal. If current behavior already passes, keep it as a regression guard.)
- [ ] **Step 3:** Run; expected PASS on current code (characterization). If it FAILS, current behavior is already wrong — note and preserve the correct order in the fix.
- [ ] **Step 4 (fix):** Replace the per-id query loop with one `SELECT ... FROM images i LEFT JOIN image_metadata m ON m.image_id = i.id WHERE i.id IN (?,?,...)` using `params_from_iter`; collect into a `HashMap<i64, RowMeta>`, then map the input `ids` slice through it to preserve exact input order (skip ids with no row, matching prior behavior).
- [ ] **Step 5:** Run the test; expected PASS. `cargo test --workspace`, `cargo clippy --workspace --all-targets`. Clean.
- [ ] **Step 6:** Strip B14 from `bughunt.md`. Commit: `fix(caching): single-query order-preserving rehydrate_rows [B14]`.

**END-OF-BATCH MILESTONE: run full `cargo test --workspace` + `cargo clippy --workspace --all-targets`. Report green.**

---

## Self-Review notes
- **Coverage:** all 10 selected findings (B1, B2, B3, B5, B6, B7, B8, B10, B11, B13, B14) have a task. B4 is skipped (recorded in skip memory + bughunt Skip section). B9, B12, B15–B19 intentionally left in `bughunt.md`.
- **B2 verified real:** `search_similar_images` returns `i.path` (relative); the main.rs comment is wrong.
- **B8 verified real:** `clamp(1, max_k)` re-caps `k` below `offset+limit`.
- **Type consistency:** batch tag fns named `batch_tag_images`/`batch_untag_images` consistently; `select_to_request` named consistently in B13.
