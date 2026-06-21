# typecheck execution spec — 2026-06-21 newtypes & enums batch

Source: `TYPECHECK.md` triage (2026-06-21). This spec covers **only** the ten
findings the user checked `[x] execute`: **T1, T2, T3, T4, T5, T6, T7, T10, T11,
T12**. (T8 GeoRect and T9 DistanceThreshold were left unchecked and stay in
`TYPECHECK.md`.)

Each finding is one commit (`typecheck(<lens>): <summary> [T<n>]`) and is
stripped from `TYPECHECK.md` in that same commit. Execution technique is
**compiler-driven migration**: introduce the type, change it at the source, let
`rustc` enumerate every break, fix to green. Do not hand-grep for sites the
compiler will find.

Ranking order (impact): T1 (9) > T2 (6) > T3/T4/T5/T6/T7 (4/4/4/4/4) > T10/T11/T12 (2/1/1).

## Verified facts (load-bearing — established by reading the code)

- **`RowMeta` (src/sort.rs:34) is NOT serde-derived** (`#[derive(Debug, Clone, PartialEq)]` only). So `RowMeta.id` (T1) and `RowMeta.size` (T7) are in-memory only — no persistence concern.
- **`ImageMetadata` (src/database.rs:1512) is NOT serde-derived** (`#[derive(Debug, Clone)]`). So the GPS pair (T4) is in-memory only — the triage's "if ever serialized" caveat does not apply.
- **The ONLY persisted-JSON crossings among selected items are:**
  - **T1:** `UiState.result_ids: Vec<i64>` and `PersistedMode::Similar(i64)` (src/ui_state.rs:49, :16).
  - **T3:** `UiState.filters: Filters` with **flat** `tags` / `tag_match` / `tags_enabled` fields (src/filters.rs:16-24), each `#[serde(default)]`.
- `Sort`/`SortKey`/`SortDir` (src/sort.rs), `GpsFilter`/`TagMatch` (src/filters.rs) are already strong enums — do NOT touch their core; T2 sits in the GUI *above* `SortKey`.
- ui_state.rs already has the test pattern to copy for round-trip pins: `round_trips_through_json` and `old_blob_without_tag_fields_deserializes` (src/ui_state.rs:72-112).

## Invariants this batch depends on (pin each with a test)

1. **The on-disk `ui_state` JSON id shape stays a bare integer.** T1's `ImageId`
   etc. MUST be `#[serde(transparent)]` so `result_ids` serializes as `[3,1,2]`
   and `Similar` as `{"kind":"similar","value":3}` exactly as before. Pin: a
   round-trip test + an "old blob with bare-int ids still deserializes" test.
2. **An older persisted `Filters` JSON (flat `tags`/`tag_match`/`tags_enabled`)
   still deserializes after T3.** T3 changes the in-memory shape; the on-disk
   representation must remain readable (compat shim). Pin: deserialize a literal
   old JSON blob and assert the tag filter is reconstructed.

---

## T1 — ID newtypes: `ImageId` / `TagId` / `CollectionId` (impact 9, effort M, risk high)
- Lens: newtype. Files: `src/database.rs`, `src/ui_state.rs`, `imgfind-gui/src/state.rs`, `imgfind-gui/src/backend.rs`, plus every site the compiler surfaces.
- **risk: high** → per the per-task contract, FIRST write characterization tests for the affected tag/collection insert+read paths (e.g. tag an image, read it back; add to collection, list it) and confirm GREEN on unchanged code, commit `test: characterize id round-trips before typecheck [T1]`. (Check for existing coverage first — `src/database.rs` tests and `imgfind-gui/src/backend.rs` tests may already cover these; if an equivalent assertion exists, cite it and skip the redundant test.)
- New types (put them where the core domain types live — `src/lib.rs` or a new `src/ids.rs` re-exported from lib): `struct ImageId(i64)`, `struct TagId(i64)`, `struct CollectionId(i64)`, each `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]` and `#[serde(transparent)]`. Add a `.0` accessor or `pub fn get(self) -> i64` plus `From<i64>`/`impl` as needed for the turso bind boundary.
- Migration: change the type at the SOURCE — DB extraction (`col_i64(..,"id")` → `ImageId`), the `images`/`tags`/`collections` insert/return signatures, `RowMeta.id`, `UiState.result_ids: Vec<ImageId>`, `PersistedMode::Similar(ImageId)`, GUI state/backend. Let rustc enumerate the rest. At the SQL bind boundary, unwrap to `i64` (turso `Value::Integer(id.get())`); inside the SELECT extraction, wrap.
- Tests: keep the two ui_state serde tests green; ADD an assertion that `serde_json::to_string` of a `UiState` with `result_ids` produces bare ints (e.g. contains `"result_ids":[3,1,2]`) and that `PersistedMode::Similar` JSON is `{"kind":"similar","value":3}` — pins invariant 1.
- Caveat: this is the widest change; expect breaks across both crates. Partial scope is allowed (the three ids are separable) but prefer one pass.

## T2 — `SortOption` enum for the GUI sort selector (impact 6, effort M, risk low)
- Lens: stringly-typed. File: `imgfind-gui/src/main.rs` (~2542, 3467, 3482) + wherever the selector model/index helpers live.
- The CORE sort is already `SortKey`/`SortDir`/`Sort` — do NOT change those. This is only the GUI selector-label layer.
- New type (in a GUI module, e.g. `imgfind-gui/src/sort_option.rs` or inline in main.rs near the selector helpers): `enum SortOption { Relevance, Name, Size, Type }` with: `impl Display` (label for the Slint string model), `fn all() -> [SortOption; 4]` (drives `make_sort_options_model` by iterating variants, not literal pushes), and `fn to_sort_key(self) -> Option<SortKey>` (`Relevance => None`, others map to the `SortKey`). Provide `fn from_label(&str) -> Option<SortOption>` only if a Slint callback hands back a string; prefer passing the selected index and mapping via `all()[idx]`.
- Migration: replace `if option_str == "Relevance"` and `match` on `"Size"/"Type"` with matches on `SortOption`; build the model from `SortOption::all()`. Keep `Relevance` as an explicit variant (it has no `SortKey`).
- Tests: unit-test `to_sort_key` for all four variants and `Display` round-trips with `from_label`/`all()` ordering (the existing `sort_sel_tests` module is the place; do not refactor it — add cases).

## T3 — `TagFilter` enum on `Filters` (impact 4, effort L [larger than triaged — serde shim], risk low)
- Lens: illegal-states. Files: `src/filters.rs`, `imgfind-gui/src/main.rs` (build_filters ~ where Filters is constructed, and the `carry_tag_filter_from` caller ~1028), `src/ui_state.rs` (the persisted boundary).
- Goal: remove the "enabled && empty tags" illegal state while (a) preserving the "tags retained while disabled for fast re-activation" feature and (b) keeping old persisted `Filters` JSON readable.
- Proposed in-memory shape on `Filters` — replace the three flat fields with:
  ```
  enum TagFilter {
      Inactive { tags: Vec<String>, match_mode: TagMatch }, // retained, not applied
      Active   { tags: Vec<String>, match_mode: TagMatch }, // applied; tags non-empty by construction
  }
  ```
  `build_filter_clause_turso` matches `Active { tags, match_mode }` (and only emits a clause when `!tags.is_empty()`, which `Active` guarantees), `Inactive => {}`. `carry_tag_filter_from` copies the whole `TagFilter`.
- **Serde compat (the load-bearing part):** the on-disk representation must stay the flat `tags`/`tag_match`/`tags_enabled` triple so existing `ui_state` rows load. Implement via a private `#[derive(Serialize, Deserialize)] struct TagFilterRepr { tags, tag_match, tags_enabled }` and `#[serde(from = "TagFilterRepr", into = "TagFilterRepr")]` on `TagFilter` (or on a wrapper), mapping `tags_enabled && !tags.is_empty()` → `Active`, else `Inactive`. This keeps `Filters`'s JSON byte-stable.
- Tests (MANDATORY, pins invariant 2): (a) deserialize a literal old blob `{"size_min":null,...,"tags":["a"],"tag_match":"anyof","tags_enabled":true}` and assert it becomes `Active`; (b) `tags_enabled:false` with non-empty tags → `Inactive` retaining the tags; (c) round-trip an `Active`/`Inactive` `Filters` through JSON and assert the flat keys are present and re-read equal. Keep ALL existing `build_filter_clause_turso` tests green (they construct `Filters{ tags, tags_enabled, tag_match, .. }` literally — those constructions must be updated to the new shape; that update is part of this task, the compiler lists them).
- If, during implementation, the serde shim + construction-site churn proves to require an architectural change beyond this seam, convert T3 to a `decision-needed` marker (do not force it).

## T4 — `GpsCoords` on `ImageMetadata` (impact 4, effort M, risk low)
- Lens: illegal-states. Files: `src/database.rs` (ImageMetadata def + insert/read ~1167, 1250, 1469, 1604, 1648, 1674), `imgfind-gui/src/detail.rs:42`.
- Replace `latitude: Option<f64>, longitude: Option<f64>` with `coords: Option<GpsCoords>` where `struct GpsCoords { lat: f64, lon: f64 }` (a plain struct; `Option<GpsCoords>` models present/absent). No serde (ImageMetadata isn't serialized).
- Migration: at the EXIF parse / DB read, construct `Some(GpsCoords{lat,lon})` only when both columns are present (the existing `if let (Some,Some)` becomes the constructor); at the insert, destructure. Collapse every paired `if let (Some(lat),Some(lon))` to `if let Some(c) = &meta.coords`. The map jitter code (`apply_stable_jitter`/`downsample_by_grid`) uses lat/lon — update those accessors.
- Tests: the existing metadata tests cover read-back; add/extend one asserting a row with only latitude in the DB yields `coords: None` (can't be half-present) — only if a test DB fixture makes this cheap; otherwise a unit test constructing the metadata mapping suffices.

## T5 — `MaxK(usize)` (impact 4, effort S, risk med)
- Lens: newtype. Files: `src/database.rs` (search fns ~759, 789, 834), `src/config.rs:37`, `imgfind-gui/src/backend.rs:93`.
- `#[derive(Debug, Clone, Copy)] #[serde(transparent)] struct MaxK(usize)` (Serialize/Deserialize for the config.toml field). Thread from `SearchConfig.max_k` through the search signatures; unwrap to `usize` only at the `limit.clamp(1, max_k.get())` site.
- Tests: existing search tests cover behavior; the type change itself prevents the `limit`/`max_k` swap. Add a tiny unit test only if `clamp` logic is extracted; otherwise rely on compile + existing tests.

## T6 — `EmbeddingDim(usize)` (impact 4, effort S, risk low)
- Lens: newtype. Files: `src/database.rs` (ModelInfo.dim ~161, 179, 196), `src/schema.rs:43, :52`.
- `#[derive(Debug, Clone, Copy, PartialEq, Eq)] struct EmbeddingDim(usize)` (serde only if `ModelInfo` is serialized — verify; the models registry is a DB table, likely not serde, so plain newtype). Carry from `ensure_and_activate_model`/model registry into `create_vector_table` and the `F32_BLOB({dim})` SQL (unwrap at the format! site).
- Tests: existing schema/model tests; type change prevents passing a non-dim usize.

## T7 — `FileSize(i64)` (impact 4, effort M, risk low)
- Lens: newtype. Files: `src/sort.rs:38` (RowMeta.size), `src/database.rs:1164, :1252`, `src/filters.rs:67-73` (size_min/size_max), `imgfind-gui/src/main.rs:2530, 2558-2567` (slider math).
- `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)] #[serde(transparent)] struct FileSize(i64)` (serde because `Filters.size_min/size_max` are persisted in ui_state — keep them bare ints on disk via transparent). `RowMeta.size: Option<FileSize>`. The GUI fraction↔bytes math unwraps to `i64`.
- NOTE on interaction with T3: `Filters` is touched by both T3 (tag fields) and T7 (size fields). Whichever runs second rebases on the first — no conflict, but the implementer should expect the other change already present.
- Tests: keep ui_state round-trip green (size bounds still serialize as bare ints — add an assertion); existing filter size tests must stay green (update their `Filters{ size_min: Some(100), .. }` literals to `Some(FileSize(100))`).

## T10 — grid-nav index newtypes (impact 2, effort M, risk med)
- Lens: newtype. Files: `imgfind-gui/src/nav.rs:36`, `imgfind-gui/src/main.rs:580-588`, `imgfind-gui/src/window.rs:38`.
- `CursorIndex(usize)`, `GridCols(usize)`, `ItemCount(usize)` (ephemeral GUI state, no serde). Change `move_selection` + `window_range` signatures; the existing nav.rs/window.rs unit tests guard the math (update their call sites, don't refactor the tests' assertions).
- Tests: existing nav/window tests; update call sites to wrap.

## T11 — `ThumbnailSize(u32)` (impact 1, effort S, risk low)
- Lens: newtype. Files: `src/thumbnail.rs:10, 44, 124, 141`, `src/database.rs:701, 1057, 1095-1098`.
- `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] struct ThumbnailSize(u32)`. `GUI_THUMBNAIL_SIZES: [ThumbnailSize; 3]`. The `thumbnails` table bind `(image_hash, size)` unwraps to `u32`.
- Tests: existing thumbnail tests; update size literals.

## T12 — `SearchState::Phase` enum (impact 1, effort M, risk low)
- Lens: illegal-states. File: `imgfind-gui/src/state.rs:17-86`.
- Replace `loading: bool` + `error: Option<String>` + `results: Vec<RowMeta>` + `has_searched: bool` with `enum Phase { Idle, Loading, Complete { results: Vec<RowMeta>, error: Option<String> } }` (ephemeral, no serde). Route the three transition methods (`start_search`/`apply_results`/`apply_error`) and `view_state` through the enum.
- NOTE interaction with T1 (RowMeta.id → ImageId) and T7 (RowMeta.size → FileSize): `Phase::Complete` holds `Vec<RowMeta>`, so T12 inherits whatever RowMeta shape the earlier tasks produced — no extra work, just rebase.
- Tests: the existing state.rs tests for `view_state`/transitions must stay green (update to construct the enum); add a test that an illegal combo is now unconstructable (compile-time — document, or assert the enum has no such variant).

---

## Execution notes
- **Ordering:** do T1 first (widest; everything rebases on the new id types), then T7 and T3 (both touch `Filters`; do T7 then T3 or vice versa, second rebases), then T4/T5/T6 (database.rs), then T2/T10/T12 (GUI), then T11. The plan may refine this.
- **Milestone test runs:** per the contract, run the full `cargo test --workspace` at every 5th finding and at batch end.
- Each task: `cargo fmt --all` + `cargo clippy --workspace --all-targets` clean; commit `typecheck(<lens>): <summary> [T<n>]` + strip the `### T<n>.` block from `TYPECHECK.md` in the same commit.

## Out of scope (left unchecked)
T8 (GeoRect), T9 (DistanceThreshold) — remain in `TYPECHECK.md`.
