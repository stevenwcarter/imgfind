# code-health Low-Findings Batch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four code-health findings the user selected (B16, B17, B18, B19) — each in its own commit, each stripped from `bughunt.md` in that commit.

**Architecture:** Four independent, surgical fixes. B16 hardens `build_filters` to keep the size range well-ordered. B17 pre-sizes three async row-drain `Vec`s. B18 makes a bad GUI config visible on stderr. B19 adds a process-wide metadata LRU so the detail panel stops re-querying the DB.

**Tech Stack:** Rust edition 2024, `imgfind` 2-crate workspace (`imgfind` core + `imgfind-gui`). `lru = "0.18"` (already a GUI dep). `turso` async SQLite. Tests via `cargo test --workspace`. Lint via `cargo clippy --workspace --all-targets`.

## Global Constraints

- Rust edition 2024; all new code must be `cargo fmt --all` clean and `cargo clippy --workspace --all-targets` clean (no new warnings).
- Errors use `anyhow` with `Context`/`with_context`. Logging via `tracing`.
- One commit per finding, format `fix(<category>): <summary> [B<n>]`.
- **Strip the fixed finding's block from `bughunt.md` in the same commit** (non-negotiable). The `## Critical/High/Medium/Low` headers stay even when a section empties.
- All Rust coding is dispatched to the `rust-developer` agent (per project memory).
- `ImageMetadata` already derives `Clone` (src/database.rs:1512). `lru` and `OnceLock` are already used in `imgfind-gui`.

---

### Task 1 (B16): Keep `Filters` size range well-ordered in `build_filters`

**Category:** api-surface · Impact 4 · Risk low

**Files:**
- Modify: `imgfind-gui/src/main.rs` — `build_filters` (around 197–224).
- Test: same file's `#[cfg(test)] mod tests` (add two unit tests).

**Interfaces:**
- Consumes: `fraction_to_bytes(fraction: f32, min: i64, max: i64, is_lo: bool) -> Option<i64>` (main.rs:142), `Filters` (`imgfind::filters::Filters`) with `size_min: Option<i64>`, `size_max: Option<i64>`.
- Produces: no signature change — `build_filters(lo, hi, size_bounds, selected_exts, gps_mode) -> Filters` still, but its result now always satisfies `size_min <= size_max` when both are `Some`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `imgfind-gui/src/main.rs`:

```rust
#[test]
fn build_filters_swaps_inverted_size_range() {
    // Inverted slider: lo fraction maps to a LARGER byte value than hi.
    // lo=0.8 -> Some(high bytes); hi=0.2 -> Some(low bytes); both Some, min>max.
    let f = build_filters(0.8, 0.2, (0, 1000), &HashSet::new(), 0);
    let (mn, mx) = (f.size_min.unwrap(), f.size_max.unwrap());
    assert!(mn <= mx, "size_min ({mn}) must be <= size_max ({mx}) after swap");
}

#[test]
fn build_filters_leaves_normal_size_range() {
    let f = build_filters(0.2, 0.8, (0, 1000), &HashSet::new(), 0);
    assert_eq!(f.size_min, Some(200));
    assert_eq!(f.size_max, Some(800));
}
```

(If `HashSet` is not already imported in the test module, use `std::collections::HashSet` inline.)

- [ ] **Step 2: Run tests to verify the swap test fails**

Run: `cargo test -p imgfind-gui build_filters_swaps_inverted_size_range`
Expected: FAIL — without the guard, `size_min` (800) > `size_max` (200), assertion fails.

- [ ] **Step 3: Add the swap guard in `build_filters`**

After the two `fraction_to_bytes` calls, before constructing `Filters`:

```rust
let size_min = fraction_to_bytes(lo, min, max, true);
let size_max = fraction_to_bytes(hi, min, max, false);
// A malformed slider state could invert lo/hi, producing size_min > size_max,
// which Filters/SQL assume never happens. Swap to keep the range well-ordered
// (swap, not drop: preserves the user's intended bounds).
let (size_min, size_max) = match (size_min, size_max) {
    (Some(a), Some(b)) if a > b => (Some(b), Some(a)),
    other => other,
};
```

- [ ] **Step 4: Run tests to verify both pass**

Run: `cargo test -p imgfind-gui build_filters`
Expected: PASS (both tests).

- [ ] **Step 5: fmt + clippy + commit (strip B16 from bughunt.md)**

```bash
cargo fmt --all
cargo clippy -p imgfind-gui --all-targets
# Remove the "### B16." block (lines 58–66 area) from bughunt.md.
git add imgfind-gui/src/main.rs bughunt.md
git commit -m "fix(api-surface): swap inverted size range in build_filters [B16]"
```

---

### Task 2 (B17): Pre-size async search-result `Vec`s

**Category:** caching · Impact 3 · Risk low

**Files:**
- Modify: `src/database.rs` — three loops: `search_similar_images` (~773), `search_similar_images_with_raw_blob` (~810), `search_similar_images_meta` (~859).

**Interfaces:**
- Consumes: local `k` / `limit` already in scope in each function. Note these are async streams (`while let Some(row) = rows.next().await?`), so `.collect()` does NOT apply — only a capacity hint is correctness-safe.
- Produces: identical row contents and order; no signature change.

- [ ] **Step 1: Apply the three capacity hints**

In `search_similar_images` (k is `usize`, clamped 1..=max_k):
```rust
let mut out = Vec::with_capacity(k);
```
In `search_similar_images_with_raw_blob` (k is `usize`):
```rust
let mut out = Vec::with_capacity(k);
```
In `search_similar_images_meta` (`limit` and `k` are `usize`):
```rust
let mut out = Vec::with_capacity(limit.min(k));
```

Leave the `while let Some(row) = rows.next().await? { out.push(...); }` bodies unchanged.

- [ ] **Step 2: Run search/database tests + build**

Run: `cargo test --workspace database`
Run: `cargo clippy --workspace --all-targets`
Expected: PASS, no new warnings. (Behavior unchanged — this is allocation-only.)

- [ ] **Step 3: fmt + commit (strip B17 from bughunt.md)**

```bash
cargo fmt --all
# Remove the "### B17." block from bughunt.md.
git add src/database.rs bughunt.md
git commit -m "fix(caching): pre-size search-result vecs with capacity hints [B17]"
```

---

### Task 3 (B18): Make a malformed GUI `config.toml` visible on stderr

**Category:** api-surface · Impact 2 · Risk low

**Files:**
- Modify: `imgfind-gui/src/main.rs` — `gui_config` load in `main` (~269–274).

**Interfaces:**
- Consumes: `imgfind::config::Config::load() -> Result<Config>`, `imgfind::config::GuiConfig::default()`.
- Produces: unchanged behavior (default fallback) plus a stderr line on parse error.

- [ ] **Step 1: Add the stderr notice on the error branch**

Replace the `unwrap_or_else` closure body:

```rust
let gui_config = imgfind::config::Config::load()
    .map(|c| c.gui)
    .unwrap_or_else(|e| {
        // Surface on stderr so a user launching from a terminal sees a bad
        // config without needing RUST_LOG; keep the default fallback.
        eprintln!("imgfind-gui: failed to load config, using defaults: {e:#}");
        tracing::warn!("Failed to load config, using defaults: {e}");
        imgfind::config::GuiConfig::default()
    });
```

- [ ] **Step 2: Build + clippy**

Run: `cargo clippy -p imgfind-gui --all-targets`
Expected: PASS, no new warnings. (No unit test — the branch is an I/O `eprintln!` with no return to assert.)

- [ ] **Step 3: fmt + commit (strip B18 from bughunt.md)**

```bash
cargo fmt --all
# Remove the "### B18." block from bughunt.md.
git add imgfind-gui/src/main.rs bughunt.md
git commit -m "fix(api-surface): print bad GUI config error to stderr [B18]"
```

---

### Task 4 (B19): Cache detail-panel metadata in a process-wide LRU

**Category:** caching · Impact 2 · Risk low

**Files:**
- Create: `imgfind-gui/src/meta_cache.rs`.
- Modify: `imgfind-gui/src/main.rs` — declare `mod meta_cache;` (near the other `mod` lines at the top, e.g. by `mod detail_cache;` at line 6) and update `spawn_detail_meta` (~3289).

**Interfaces:**
- Consumes: `imgfind::database::ImageMetadata` (re-exported as `imgfind::ImageMetadata`? — use the path the crate already uses; backend.rs imports it). `lru::LruCache`, `std::sync::Mutex`, `std::sync::OnceLock` (or `LazyLock`).
- Produces:
  - `meta_cache::get(key: &str) -> Option<ImageMetadata>` — clones out, promotes LRU.
  - `meta_cache::insert(key: String, meta: ImageMetadata)`.

- [ ] **Step 1: Write `meta_cache.rs` with a round-trip unit test**

Create `imgfind-gui/src/meta_cache.rs`. Mirror `detail_cache.rs`'s doc style, but use a `Send` `Mutex<LruCache>` (the metadata read runs on a background thread, unlike the UI-thread-only `detail_cache`):

```rust
//! Process-wide bounded LRU of detail-panel image metadata, keyed by relative
//! path.
//!
//! Unlike `detail_cache` (UI-thread-only, holds `!Send` `slint::Image`), the
//! metadata read in `spawn_detail_meta` runs on a background thread, so this
//! cache must be `Send`: a `Mutex<LruCache<String, ImageMetadata>>` behind a
//! `OnceLock`. An indexed image's metadata is stable for the lifetime of the
//! GUI (re-indexing is a separate process), so no generation-bump invalidation
//! is needed.

use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};

use imgfind::database::ImageMetadata;
use lru::LruCache;

/// Number of metadata records held. Metadata structs are tiny, so this can be
/// generous relative to a browsing run.
const META_CACHE_CAPACITY: usize = 128;

fn cache() -> &'static Mutex<LruCache<String, ImageMetadata>> {
    static CACHE: OnceLock<Mutex<LruCache<String, ImageMetadata>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(META_CACHE_CAPACITY).expect("META_CACHE_CAPACITY is non-zero"),
        ))
    })
}

/// Cached metadata for `key` (relative path), if present; promotes it to MRU.
pub fn get(key: &str) -> Option<ImageMetadata> {
    cache().lock().unwrap().get(key).cloned()
}

/// Insert (or refresh) metadata for `key`.
pub fn insert(key: String, meta: ImageMetadata) {
    cache().lock().unwrap().put(key, meta);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_miss() {
        let meta = ImageMetadata {
            file_size: Some(123),
            width: Some(4),
            height: Some(2),
            latitude: None,
            longitude: None,
            camera_make: Some("Sony".into()),
            camera_model: None,
            datetime_taken: None,
        };
        insert("meta_cache_test_key.jpg".into(), meta.clone());
        let got = get("meta_cache_test_key.jpg").expect("present after insert");
        assert_eq!(got.file_size, meta.file_size);
        assert_eq!(got.camera_make, meta.camera_make);
        assert!(get("meta_cache_absent_key.jpg").is_none());
    }
}
```

Verify the `ImageMetadata` field list against `src/database.rs:1513` and the correct import path (match how `backend.rs` imports `ImageMetadata`). Adjust the literal to include every field exactly once.

- [ ] **Step 2: Declare the module**

In `imgfind-gui/src/main.rs`, add near `mod detail_cache;` (line 6):
```rust
mod meta_cache;
```

- [ ] **Step 3: Run the cache test (RED→GREEN for the new module)**

Run: `cargo test -p imgfind-gui meta_cache`
Expected: PASS (round_trip_and_miss).

- [ ] **Step 4: Wire `spawn_detail_meta` through the cache**

In `spawn_detail_meta` (~3289), check the cache first and populate on miss:

```rust
fn spawn_detail_meta(
    weak: Weak<MainWindow>,
    backend: Backend,
    detail: Arc<Mutex<Option<DetailState>>>,
    path: String,
) {
    std::thread::spawn(move || {
        let meta_result = match meta_cache::get(&path) {
            Some(meta) => Ok(meta),
            None => {
                let r = backend.metadata(&path);
                if let Ok(ref meta) = r {
                    meta_cache::insert(path.clone(), meta.clone());
                }
                r
            }
        };
        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            if !detail_shows(&detail.lock(), &path) {
                return;
            }
            match meta_result {
                Ok(meta) => w.set_detail_meta(format_metadata(&meta).into()),
                Err(e) => {
                    tracing::warn!("detail metadata failed: {e}");
                    w.set_detail_meta("".into());
                }
            }
        })
        .ok();
    });
}
```

Note: `Backend::metadata` returns `anyhow::Result<ImageMetadata>`; only `Ok` results are cached, so a transient error is retried next open.

- [ ] **Step 5: Build, test, clippy**

Run: `cargo test -p imgfind-gui`
Run: `cargo clippy --workspace --all-targets`
Expected: PASS, no new warnings.

- [ ] **Step 6: fmt + commit (strip B19 from bughunt.md)**

```bash
cargo fmt --all
# Remove the "### B19." block from bughunt.md (last finding under ## Low).
git add imgfind-gui/src/meta_cache.rs imgfind-gui/src/main.rs bughunt.md
git commit -m "fix(caching): cache detail-panel metadata in a process-wide LRU [B19]"
```

---

### Final verification

- [ ] Run the full suite: `cargo test --workspace` — expect green.
- [ ] Run `cargo clippy --workspace --all-targets` — expect no new warnings.
- [ ] Confirm `bughunt.md` no longer contains B16/B17/B18/B19 (only B9, B12, B15 and the Skip section remain).

## Self-Review notes
- **Spec coverage:** B16→Task 1, B17→Task 2, B18→Task 3, B19→Task 4. All four selected findings covered; B9/B12/B15 correctly out of scope.
- **Type consistency:** `meta_cache::get`/`insert` signatures used identically in Task 4 Step 1 and Step 4. `ImageMetadata` field list to be verified against database.rs during implementation.
- **Async caveat:** Task 2 explicitly avoids the triage's `.collect()` suggestion (invalid for async streams) — capacity hint only.
