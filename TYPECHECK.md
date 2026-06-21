# TYPECHECK.md — type-system strengthening findings

Last triage: 2026-06-21 against `main` @ 20d80f38. Toolchain: cargo build/check --workspace / cargo test --workspace / cargo clippy --workspace --all-targets.

> When you fix an item here, strip it from this file in the same commit that fixes it (open issues only).

## How to use this file
- Check `[x] execute` to fix this batch; `[x] skip` to never re-flag; leave unchecked to keep for next run.
- Ranking is impact = bug-prevention × blast-radius (effort is separate, never folded into rank).
- Renames are flag-only (decision-needed), never auto-applied. Public-API type changes ARE in scope.
- When ready, run `/typecheck --execute`.

## Critical

## High

### T2. UI sort-selector strings sit un-typed above the existing `SortKey` enum: `make_sort_options_model` / `option_str_to_sort_key` (imgfind-gui/src/main.rs:2542, :3467, :3482)
- **Lens:** stringly-typed
- **Impact:** 6 (bug-prevention high × blast 2)
- **Effort:** M
- **Risk:** low
- **Current type:** `String`/`SharedString` selector labels. NOTE: the *core* sort is already strongly typed (`SortKey { Name, Size, Type }` + `SortDir` + `Sort` in src/sort.rs); this finding is strictly the **GUI selector-string layer that maps onto `SortKey` plus a `Relevance` special case** — not "sort is stringly-typed" at the core (it isn't).
- **Proposed type:** `enum SortOption { Relevance, Name, Size, Type }` with `Display` (for the Slint string model) and a total mapping to `Option<SortKey>` (Relevance → None / the relevance-order special case). The selector model and `*_to_selector_index` helpers key off the enum instead of label strings.
- **Evidence:** option labels are compared with `if option_str == "Relevance"` and `match` on `"Size"`/`"Type"`, while `make_sort_options_model` pushes `"Relevance"`/`"Name"`/`"Size"`/`"Type"` in a fixed order. The model order and the `sort_key_to_browse_index` / `sort_to_selector_index` integer indices are tightly coupled by hand — a label typo or a reorder of the pushes silently mismaps the dropdown selection to the wrong sort, and `"Relevance"` (which has no `SortKey`) is an unguarded string special-case.
- **Blast radius:** imgfind-gui/src/main.rs:2542 (`if option_str == "Relevance"`), :3470 (push `"Relevance"`), :3472-3474 (push Name/Size/Type), :3484-3485 (`match "Size"/"Type"`).
- **Invariants/caveats:** Purely UI — no serde/DB boundary crossed (the persisted sort goes through the typed `Sort`). Safe to do independently of T1.
- **Proposed migration:** introduce `SortOption` with `Display` + `to_sort_key()`; build the Slint model by iterating the enum variants; replace the string compares/matches at source and fix breaks to green.
- [x] execute   [ ] skip

## Medium

### T6. Embedding dimension is a bare `usize` that must equal the model's true dim: `embedding dimension` (src/database.rs:161)
- **Lens:** newtype
- **Impact:** 4 (bug-prevention med × blast 2)
- **Effort:** S
- **Risk:** low
- **Current type:** `usize` (512 / 768).
- **Proposed type:** `struct EmbeddingDim(usize)`.
- **Evidence:** the dim flows from the model registry into `F32_BLOB(dim)` schema creation and vector-table sizing; a wrong dim produces malformed embeddings / mismatched vector tables. Rarely transposed in practice, but it's a load-bearing correctness invariant currently indistinguishable from any other count.
- **Blast radius:** src/database.rs:161, :179, :196; src/schema.rs:43, :52.
- **Invariants/caveats:** None serde-facing (derived from the active model at runtime). Pairs conceptually with the per-model `F32_BLOB(dim)` invariant.
- **Proposed migration:** introduce `EmbeddingDim(usize)`; carry it from `ensure_and_activate_model` through schema/vector-table creation; fix to green.
- [x] execute   [ ] skip

### T8. Four adjacent `f64` bounds invite N/S/E/W swaps: `geographic bounds` (src/database.rs:1211)
- **Lens:** newtype
- **Impact:** 3 (bug-prevention med × blast 1, but a real silent-wrong-result class)
- **Effort:** S
- **Risk:** med
- **Current type:** four adjacent `f64` params (north, south, east, west).
- **Proposed type:** `struct GeoRect { north: f64, south: f64, east: f64, west: f64 }` (consider a constructor that validates `south <= north`, `west <= east`).
- **Evidence:** four same-typed adjacent params; swapping N/S or E/W compiles and **silently queries the wrong rectangle**. `get_images_by_bounds` is a library fn with **no current GUI caller** (no map view yet), so user-facing risk is low today — but it's the highest-smell signature in the DB layer and will be wired up when the map view lands.
- **Blast radius:** src/database.rs:1211-1221, :1235-1238.
- **Invariants/caveats:** None serde-facing. Low blast precisely because there's no caller yet — cheap to fix before one exists.
- **Proposed migration:** introduce `GeoRect`; change the fn signature; (no caller to fix); add a constructor invariant + unit test.
- [ ] execute   [ ] skip

## Low

### T9. `distance threshold` is a bare `f32` (documentation-grade): src/database.rs:758
- **Lens:** newtype
- **Impact:** 2 (bug-prevention med × blast 1)
- **Effort:** S
- **Risk:** low
- **Current type:** `f32`.
- **Proposed type:** `struct DistanceThreshold(f32)`.
- **Evidence:** sits adjacent to `max_k` but is a different primitive type, so transposition is already prevented by the compiler. Value is semantic clarity (it's a cosine distance in [0, 2], threshold ≤ 1.3), not bug prevention.
- **Blast radius:** src/database.rs:754-760, :783-789, :828-834, :878-881; src/config.rs:34; imgfind-gui/src/backend.rs:86.
- **Invariants/caveats:** Crosses config (serde) — `#[serde(transparent)]` to keep config.toml readable. Could encode the `0.0..=2.0` range as a constructor invariant.
- **Proposed migration:** introduce `DistanceThreshold(f32)`; thread from config; fix to green.
- [ ] execute   [ ] skip

### T10. Grid nav indices share `usize` (test-covered, documentation-grade): `cursor / cols / len` (imgfind-gui/src/nav.rs:36)
- **Lens:** newtype
- **Impact:** 2 (bug-prevention med × blast 1)
- **Effort:** M
- **Risk:** med
- **Current type:** three `usize` (cursor, cols, len).
- **Proposed type:** `CursorIndex` / `GridCols` / `ItemCount` newtypes.
- **Evidence:** multiple same-typed `usize`; swapping `cols` and `len` breaks grid math. Mitigated: `move_selection` is covered by nav.rs/window.rs unit tests that would catch a swap, so value is mostly clarity.
- **Blast radius:** imgfind-gui/src/nav.rs:36; imgfind-gui/src/main.rs:580-588; imgfind-gui/src/window.rs:38.
- **Invariants/caveats:** Ephemeral grid state (not persisted) — no serde concern.
- **Proposed migration:** introduce the three index newtypes; change `move_selection` + window signatures; fix to green; existing tests guard correctness.
- [x] execute   [ ] skip

### T11. Thumbnail size is a bare `u32` (documentation-grade): `thumbnail size (px)` (src/thumbnail.rs:44)
- **Lens:** newtype
- **Impact:** 1 (bug-prevention low × blast 1)
- **Effort:** S
- **Risk:** low
- **Current type:** `u32` (px).
- **Proposed type:** `struct ThumbnailSize(u32)`.
- **Evidence:** distinct type from the `i64` file size, so transposition risk is low; sizes are mostly the const `GUI_THUMBNAIL_SIZES`. Documentation value.
- **Blast radius:** src/thumbnail.rs:10, :44, :124, :141; src/database.rs:701, :1057, :1095-1098.
- **Invariants/caveats:** Keyed into the `thumbnails` table `(image_hash, size)` — a newtype is fine as long as the bind value stays the inner `u32`.
- **Proposed migration:** introduce `ThumbnailSize(u32)`; change the thumbnail/DB signatures; fix to green.
- [x] execute   [ ] skip

### T12. `SearchState` 4-field truth table allows illegal combos (well-guarded today): (imgfind-gui/src/state.rs:17-29)
- **Lens:** illegal-states
- **Impact:** 1 (bug-prevention low × blast 1)
- **Effort:** M
- **Risk:** low
- **Current type:** `loading: bool` + `error: Option<String>` + `results: Vec<…>` + `has_searched: bool` (a 5-state truth table with illegal combos like `loading && error`, `loading && results`, `has_searched == false` with results).
- **Proposed type:** `enum Phase { Idle, Loading, Complete { results, error } }`.
- **Evidence:** illegal combinations are *constructible* by direct field mutation, but in practice all transitions go through 3 well-guarded methods and `view_state`, so no live bug. Lens recommended deferring.
- **Blast radius:** imgfind-gui/src/state.rs:37-42, :49-53, :56-60, :74-86 (`view_state`).
- **Invariants/caveats:** Ephemeral GUI state — not persisted, no serde concern.
- **Proposed migration:** introduce `Phase`; replace the four fields; route the 3 transition methods + `view_state` through the enum; fix to green.
- [x] execute   [ ] skip
