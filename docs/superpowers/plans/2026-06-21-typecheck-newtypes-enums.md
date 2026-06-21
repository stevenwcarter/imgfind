# Typecheck Newtypes & Enums Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Strengthen `imgfind`'s types per the 10 selected `TYPECHECK.md` findings (T1–T7, T10–T12): ID newtypes, a GUI sort-option enum, a tag-filter sum type, GPS-coord pairing, and several value newtypes — each making a class of bug fail to compile.

**Architecture:** Compiler-driven migration. For each finding: introduce the new type, change it at the source (field/param/return), run `cargo build --workspace` and let `rustc` enumerate every break, fix to green. One commit per finding, stripped from `TYPECHECK.md` in the same commit. Two findings cross the persisted `ui_state` JSON boundary (T1, T3) and carry mandatory round-trip tests.

**Tech Stack:** Rust edition 2024, 2-crate workspace (`imgfind` core in `src/`, `imgfind-gui`). serde (JSON ui_state + TOML config), turso (async SQLite). Tests: `cargo test --workspace`. Lint: `cargo clippy --workspace --all-targets`.

**Full detail:** `docs/superpowers/specs/2026-06-21-typecheck-newtypes-enums-design.md` — read the matching `## T<n>` section before each task; it carries the verified facts and invariants.

## Global Constraints

- Rust edition 2024; every commit `cargo fmt --all` clean and `cargo clippy --workspace --all-targets` clean (no new warnings).
- Errors use `anyhow` with `Context`. All `Database` methods are `async`; sync callers use `imgfind::block_on`.
- **Compiler-driven:** do NOT hand-grep for call sites the compiler will surface — `cargo build --workspace` is the to-do list.
- One commit per finding: `typecheck(<lens>): <summary> [T<n>]`; **strip the `### T<n>.` block from `TYPECHECK.md` in that same commit.** Section headers (`## Critical/High/Medium/Low`) stay even when emptied.
- Append this trailer to every commit body: `Claude-Session: https://claude.ai/code/session_01SxaLWT95j5k1MnfFXNUkfA`
- All Rust coding dispatched to the `rust-developer` agent (project memory).
- **Persisted-JSON invariants (pin with tests):** (1) ui_state id fields stay bare integers (T1 newtypes are `#[serde(transparent)]`); (2) old persisted `Filters` JSON (flat `tags`/`tag_match`/`tags_enabled`) still deserializes after T3 (serde compat shim).
- **One-way test rule:** do not refactor existing `#[cfg(test)]` assertions; update their construction call sites to the new types, and ADD new tests where required.
- **Milestone:** run full `cargo test --workspace` after every 5th completed finding and at batch end.

**Execution order (each rebases on the previous):** T1 → T7 → T3 → T4 → T5 → T6 → T2 → T10 → T12 → T11.

---

### Task 1 (T1): ID newtypes `ImageId` / `TagId` / `CollectionId`

**Spec:** `## T1`. **Lens:** newtype. **Risk: high** — characterization tests first.

**Files:**
- Create: `src/ids.rs` (the three newtypes); declare `pub mod ids;` + re-export in `src/lib.rs`.
- Modify: `src/database.rs` (id extraction, insert/return signatures, image_tags/collection_images), `src/sort.rs` (`RowMeta.id`), `src/ui_state.rs` (`result_ids`, `PersistedMode::Similar`), `imgfind-gui/src/state.rs`, `imgfind-gui/src/backend.rs`, and every site the compiler surfaces.
- Test: `src/ids.rs` (transparent-serde unit test), `src/ui_state.rs` tests (extend), plus characterization tests in `src/database.rs` test module if not already covered.

**Interfaces:**
- Produces: `imgfind::ids::ImageId(i64)`, `TagId(i64)`, `CollectionId(i64)` — each `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(transparent)]`, with `pub const fn get(self) -> i64` and `impl From<i64>`. Used by Tasks 9 (T12 holds `Vec<RowMeta>`).

- [ ] **Step 1: Characterize the id round-trip paths (risk: high)**

First check existing coverage in `src/database.rs` tests and `imgfind-gui/src/backend.rs` tests for: tag an image → read tags back; add image to collection → list collection. If equivalent assertions already exist, cite them in the report and skip to Step 2. Otherwise write a characterization test (DB-backed, using the existing test harness) that tags an image and reads it back, confirm it PASSES on unchanged code, and commit:
```bash
git commit -m "test: characterize id round-trips before typecheck [T1]"
```

- [ ] **Step 2: Create `src/ids.rs`**

```rust
//! Strongly-typed row identifiers. Newtypes over `i64` so an image id, tag id,
//! and collection id can never be transposed (the `image_tags` /
//! `collection_images` inserts take two adjacent ids). `#[serde(transparent)]`
//! keeps the persisted `ui_state` JSON a bare integer.
use serde::{Deserialize, Serialize};

macro_rules! row_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub i64);
        impl $name {
            pub const fn get(self) -> i64 { self.0 }
        }
        impl From<i64> for $name {
            fn from(v: i64) -> Self { Self(v) }
        }
    };
}
row_id!(ImageId);
row_id!(TagId);
row_id!(CollectionId);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_id_serializes_transparently() {
        assert_eq!(serde_json::to_string(&ImageId(7)).unwrap(), "7");
        let v: Vec<ImageId> = serde_json::from_str("[3,1,2]").unwrap();
        assert_eq!(v, vec![ImageId(3), ImageId(1), ImageId(2)]);
    }
}
```
Add `pub mod ids;` and `pub use ids::{ImageId, TagId, CollectionId};` to `src/lib.rs`.

- [ ] **Step 3: Change types at the source, then let the compiler drive**

Change `RowMeta.id: ImageId`, `UiState.result_ids: Vec<ImageId>`, `PersistedMode::Similar(ImageId)`, and the DB extraction/insert/return signatures for image/tag/collection ids. Run `cargo build --workspace` and fix every reported break. At turso bind sites use `Value::Integer(id.get())`; at SELECT extraction wrap `ImageId(col_i64(...)?)`.

- [ ] **Step 4: Pin the serde invariant**

Extend `src/ui_state.rs` tests: in `round_trips_through_json` the `result_ids` are now `vec![ImageId(3), ImageId(1), ImageId(2)]`; ADD asserts that the serialized JSON contains `"result_ids":[3,1,2]` and that `PersistedMode::Similar(ImageId(3))` serializes to `{"kind":"similar","value":3}`. Confirm `old_blob_without_tag_fields_deserializes` still passes (bare-int ids).

- [ ] **Step 5: Build, test, lint, commit + strip**

```bash
cargo build --workspace && cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets
# strip the ### T1. block from TYPECHECK.md
git add -A && git commit -m "typecheck(newtype): ImageId/TagId/CollectionId row-id newtypes [T1]"
```
Expected: green workspace; ui_state JSON byte-stable.

---

### Task 2 (T7): `FileSize(i64)` newtype

**Spec:** `## T7`. **Lens:** newtype. Risk low. (Done before T3 because both touch `Filters`; this one is mechanical.)

**Files:**
- Create/extend: add `FileSize` to `src/ids.rs` (or a `src/units.rs` — keep value newtypes together; pick `src/units.rs` to avoid mixing ids and units, and re-export from lib).
- Modify: `src/sort.rs:38` (`RowMeta.size`), `src/database.rs:1164, :1252`, `src/filters.rs:10-11` (`size_min`/`size_max`), `imgfind-gui/src/main.rs` (slider fraction↔bytes ~2530, 2558-2567 and `build_filters`).
- Test: `src/units.rs` unit test; keep ui_state + filter size tests green.

**Interfaces:**
- Consumes: nothing from T1.
- Produces: `imgfind::units::FileSize(i64)` — `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)] #[serde(transparent)]`, `pub const fn bytes(self) -> i64`.

- [ ] **Step 1: Create the newtype**

```rust
//! Domain value newtypes (non-id). `FileSize` is bytes; `#[serde(transparent)]`
//! keeps the persisted `Filters.size_min/size_max` JSON bare integers.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileSize(pub i64);
impl FileSize {
    pub const fn bytes(self) -> i64 { self.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_size_is_transparent_bytes() {
        assert_eq!(serde_json::to_string(&FileSize(1024)).unwrap(), "1024");
        assert!(FileSize(10) < FileSize(20));
    }
}
```
Add `pub mod units;` + `pub use units::FileSize;` to `src/lib.rs`.

- [ ] **Step 2: Change at source + compiler-drive**

`RowMeta.size: Option<FileSize>`, `Filters.size_min/size_max: Option<FileSize>`. `cargo build --workspace`; fix breaks. At the SQL bind in `build_filter_clause_turso` use `Value::Integer(min.bytes())`; the GUI fraction↔bytes math unwraps via `.bytes()` / wraps via `FileSize(..)`.

- [ ] **Step 3: Update existing test literals + pin serde**

Update `src/filters.rs` size tests (`size_min: Some(100)` → `Some(FileSize(100))`, and the expected `Value::Integer(100)` stays — verify the bind still emits the bare int). In `src/ui_state.rs`, the filters size bounds still serialize as bare ints — confirm `round_trips_through_json` passes.

- [ ] **Step 4: Build, test, lint, commit + strip**

```bash
cargo build --workspace && cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T7. block
git add -A && git commit -m "typecheck(newtype): FileSize byte newtype [T7]"
```

---

### Task 3 (T3): `TagFilter` sum type on `Filters`

**Spec:** `## T3` (effort L — serde compat shim). **Lens:** illegal-states. Risk low.

**Files:**
- Modify: `src/filters.rs` (replace 3 flat fields with `tag_filter: TagFilter` + `TagFilterRepr` serde shim; update `build_filter_clause_turso`, `carry_tag_filter_from`), `imgfind-gui/src/main.rs` (Filters construction in `build_filters` + the `carry_tag_filter_from` caller ~1028), `src/ui_state.rs` (persisted boundary).
- Test: `src/filters.rs` tests (update constructions; ADD old-blob deserialize + shape round-trip).

**Interfaces:**
- Consumes: `FileSize` from Task 2 (the size fields are already `Option<FileSize>`).
- Produces: `Filters.tag_filter: TagFilter` where `enum TagFilter { Inactive { tags: Vec<String>, match_mode: TagMatch }, Active { tags: Vec<String>, match_mode: TagMatch } }`.

- [ ] **Step 1: Write the failing back-compat test**

Add to `src/filters.rs` tests:
```rust
#[test]
fn old_flat_filters_json_deserializes_into_tag_filter() {
    // Pre-migration on-disk shape: flat tags/tag_match/tags_enabled.
    let json = r#"{"size_min":null,"size_max":null,"extensions":[],"gps":"any","tags":["a"],"tag_match":"anyof","tags_enabled":true}"#;
    let f: Filters = serde_json::from_str(json).unwrap();
    match &f.tag_filter {
        TagFilter::Active { tags, match_mode } => {
            assert_eq!(tags, &vec!["a".to_string()]);
            assert_eq!(*match_mode, TagMatch::AnyOf);
        }
        _ => panic!("expected Active"),
    }
}
#[test]
fn disabled_with_tags_is_inactive_retaining_them() {
    let json = r#"{"size_min":null,"size_max":null,"extensions":[],"gps":"any","tags":["a","b"],"tag_match":"allof","tags_enabled":false}"#;
    let f: Filters = serde_json::from_str(json).unwrap();
    match &f.tag_filter {
        TagFilter::Inactive { tags, .. } => assert_eq!(tags.len(), 2),
        _ => panic!("expected Inactive retaining tags"),
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p imgfind filters::tests::old_flat_filters_json_deserializes_into_tag_filter`
Expected: FAIL to compile (`tag_filter`/`TagFilter` don't exist yet).

- [ ] **Step 3: Implement `TagFilter` + serde shim**

In `src/filters.rs`:
```rust
#[derive(Clone, Debug, PartialEq)]
pub enum TagFilter {
    Inactive { tags: Vec<String>, match_mode: TagMatch },
    Active { tags: Vec<String>, match_mode: TagMatch },
}
impl Default for TagFilter {
    fn default() -> Self { TagFilter::Inactive { tags: Vec::new(), match_mode: TagMatch::default() } }
}
impl TagFilter {
    /// Tags to apply, or empty when not active.
    pub fn active_tags(&self) -> Option<(&[String], TagMatch)> {
        match self {
            TagFilter::Active { tags, match_mode } if !tags.is_empty() => Some((tags, *match_mode)),
            _ => None,
        }
    }
}

// On-disk representation: the historical flat triple. Keeps ui_state JSON stable.
#[derive(Serialize, Deserialize)]
struct TagFilterRepr {
    #[serde(default)] tags: Vec<String>,
    #[serde(default)] tag_match: TagMatch,
    #[serde(default)] tags_enabled: bool,
}
impl From<TagFilterRepr> for TagFilter {
    fn from(r: TagFilterRepr) -> Self {
        if r.tags_enabled && !r.tags.is_empty() {
            TagFilter::Active { tags: r.tags, match_mode: r.tag_match }
        } else {
            TagFilter::Inactive { tags: r.tags, match_mode: r.tag_match }
        }
    }
}
impl From<&TagFilter> for TagFilterRepr {
    fn from(t: &TagFilter) -> Self {
        let (tags, match_mode, enabled) = match t {
            TagFilter::Active { tags, match_mode } => (tags.clone(), *match_mode, true),
            TagFilter::Inactive { tags, match_mode } => (tags.clone(), *match_mode, false),
        };
        TagFilterRepr { tags, tag_match: match_mode, tags_enabled: enabled }
    }
}
```
Replace the three `Filters` fields with a single `pub tag_filter: TagFilter`, flattened on-disk via the repr. Because `Filters` derives `Serialize/Deserialize` and the three flat keys must stay top-level, implement the shim with `#[serde(flatten)]` on a `TagFilterRepr`-typed shadow OR a manual `Serialize`/`Deserialize` for `Filters`. Simplest: keep `Filters` deriving serde but make `tag_filter` use `#[serde(flatten, with = "tag_filter_serde")]` — if `flatten` + `from/into` proves awkward, implement `Serialize`/`Deserialize` for `Filters` by hand mapping to a fully-flat `FiltersRepr { size_min, size_max, extensions, gps, tags, tag_match, tags_enabled }`. Update `build_filter_clause_turso` to `if let Some((tags, match_mode)) = f.tag_filter.active_tags() { ... }`, and `carry_tag_filter_from` to copy `self.tag_filter = other.tag_filter.clone()`.

- [ ] **Step 4: Compiler-drive the construction sites**

`cargo build --workspace`. Fix every `Filters { tags, tag_match, tags_enabled, .. }` construction (in `imgfind-gui/src/main.rs` `build_filters` and existing filter tests) to set `tag_filter: TagFilter::...`. The GUI `ft`/tag-load chords that flipped `tags_enabled` now construct/replace `tag_filter`.

- [ ] **Step 5: Run all filter + ui_state tests**

Run: `cargo test -p imgfind filters:: ui_state::`
Expected: PASS including the two new back-compat tests. If the serde shim cannot keep the flat on-disk shape without an architectural change, STOP and convert T3 to a `decision-needed` marker in `TYPECHECK.md` (do not force it).

- [ ] **Step 6: Lint, commit + strip**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T3. block
git add -A && git commit -m "typecheck(illegal-states): TagFilter sum type on Filters [T3]"
```

---

### Task 4 (T4): `GpsCoords` on `ImageMetadata`

**Spec:** `## T4`. **Lens:** illegal-states. Risk low. (No serde — `ImageMetadata` is not serialized.)

**Files:**
- Modify: `src/database.rs` (`ImageMetadata` def ~1513, insert ~1167, read ~1250, ~1469, EXIF parse ~1604, jitter ~1648/1674), `imgfind-gui/src/detail.rs:42`.
- Test: `src/database.rs` test module (extend metadata mapping test).

**Interfaces:**
- Produces: `ImageMetadata.coords: Option<GpsCoords>` where `pub struct GpsCoords { pub lat: f64, pub lon: f64 }` (`#[derive(Debug, Clone, Copy, PartialEq)]`).

- [ ] **Step 1: Introduce the struct + change the field**

In `src/database.rs`, add `#[derive(Debug, Clone, Copy, PartialEq)] pub struct GpsCoords { pub lat: f64, pub lon: f64 }`. Replace `latitude: Option<f64>, longitude: Option<f64>` with `coords: Option<GpsCoords>`.

- [ ] **Step 2: Compiler-drive read/insert/jitter/detail**

`cargo build --workspace`. At the EXIF parse and DB read, build `coords: (lat.zip(lon)).map(|(lat, lon)| GpsCoords { lat, lon })` (replaces the paired `if let`). At insert, destructure `meta.coords.map(|c| c.lat)` for each column bind. Collapse `imgfind-gui/src/detail.rs:42`'s `if let (Some(lat), Some(lon))` to `if let Some(c) = &meta.coords`. Update `apply_stable_jitter`/`downsample_by_grid` lat/lon access.

- [ ] **Step 3: Test the no-half-coordinate property**

Extend the metadata test: construct/read a row with only latitude present and assert `coords` is `None` (half-present is unrepresentable). Use the existing DB test harness; if a fixture is expensive, a unit test on the parse mapping suffices.

- [ ] **Step 4: Build, test, lint, commit + strip**

```bash
cargo build --workspace && cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T4. block
git add -A && git commit -m "typecheck(illegal-states): GpsCoords pairs lat/lon on ImageMetadata [T4]"
```

---

### Task 5 (T5): `MaxK(usize)` newtype

**Spec:** `## T5`. **Lens:** newtype. Risk med.

**Files:**
- Modify: add `MaxK` to `src/units.rs`; `src/database.rs` (search fns ~759, 789, 834), `src/config.rs:37`, `imgfind-gui/src/backend.rs:93`.

**Interfaces:**
- Produces: `imgfind::units::MaxK(usize)` — `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(transparent)]`, `pub const fn get(self) -> usize`.

- [ ] **Step 1: Add the newtype**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaxK(pub usize);
impl MaxK { pub const fn get(self) -> usize { self.0 } }
```
Re-export from `src/lib.rs`.

- [ ] **Step 2: Change `SearchConfig.max_k: MaxK`, thread through, compiler-drive**

`cargo build --workspace`; at `let k = limit.clamp(1, max_k.get())` unwrap. Fix the GUI/backend call sites.

- [ ] **Step 3: Build, test, lint, commit + strip**
```bash
cargo build --workspace && cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T5. block
git add -A && git commit -m "typecheck(newtype): MaxK search-cap newtype [T5]"
```
Note: this is the 5th completed finding → the `cargo test --workspace` here is the milestone run.

---

### Task 6 (T6): `EmbeddingDim(usize)` newtype

**Spec:** `## T6`. **Lens:** newtype. Risk low.

**Files:**
- Modify: add `EmbeddingDim` to `src/units.rs`; `src/database.rs` (`ModelInfo.dim` ~161, ~179, ~196), `src/schema.rs:43, :52`.

**Interfaces:**
- Produces: `imgfind::units::EmbeddingDim(usize)` — `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, `pub const fn get(self) -> usize`. Add `Serialize/Deserialize + transparent` ONLY if `ModelInfo` is serde-derived (verify; likely not — it's a DB row).

- [ ] **Step 1: Add the newtype** (plain, no serde unless ModelInfo needs it)
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingDim(pub usize);
impl EmbeddingDim { pub const fn get(self) -> usize { self.0 } }
```

- [ ] **Step 2: Change `ModelInfo.dim: EmbeddingDim`, thread to schema, compiler-drive**

`cargo build --workspace`; at the `F32_BLOB({})` format and vector-table sizing, unwrap via `.get()`.

- [ ] **Step 3: Build, test, lint, commit + strip**
```bash
cargo build --workspace && cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T6. block
git add -A && git commit -m "typecheck(newtype): EmbeddingDim model-dimension newtype [T6]"
```

---

### Task 7 (T2): `SortOption` enum for the GUI selector

**Spec:** `## T2`. **Lens:** stringly-typed. Risk low.

**Files:**
- Create: `imgfind-gui/src/sort_option.rs`; declare `mod sort_option;` in `imgfind-gui/src/main.rs`.
- Modify: `imgfind-gui/src/main.rs` (~2542, 3467-3485 selector model + index helpers).
- Test: `imgfind-gui/src/sort_option.rs` unit tests (and the existing `sort_sel_tests` — add cases, don't refactor).

**Interfaces:**
- Consumes: `imgfind::sort::SortKey`.
- Produces: `enum SortOption { Relevance, Name, Size, Type }` with `fn all() -> [SortOption; 4]`, `impl Display`, `fn to_sort_key(self) -> Option<SortKey>`.

- [ ] **Step 1: Write `sort_option.rs` with tests**
```rust
//! GUI sort-selector option: the label layer above the core `SortKey`.
//! `Relevance` has no `SortKey` (it restores relevance order).
use imgfind::sort::SortKey;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOption { Relevance, Name, Size, Type }

impl SortOption {
    pub fn all() -> [SortOption; 4] {
        [SortOption::Relevance, SortOption::Name, SortOption::Size, SortOption::Type]
    }
    pub fn to_sort_key(self) -> Option<SortKey> {
        match self {
            SortOption::Relevance => None,
            SortOption::Name => Some(SortKey::Name),
            SortOption::Size => Some(SortKey::Size),
            SortOption::Type => Some(SortKey::Type),
        }
    }
}
impl fmt::Display for SortOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SortOption::Relevance => "Relevance",
            SortOption::Name => "Name",
            SortOption::Size => "Size",
            SortOption::Type => "Type",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn to_sort_key_maps_all_variants() {
        assert_eq!(SortOption::Relevance.to_sort_key(), None);
        assert_eq!(SortOption::Size.to_sort_key(), Some(SortKey::Size));
    }
    #[test]
    fn display_matches_labels() {
        assert_eq!(SortOption::all().map(|o| o.to_string()),
            ["Relevance", "Name", "Size", "Type"].map(String::from));
    }
}
```

- [ ] **Step 2: Build the selector model from `SortOption::all()` and replace string matches**

`cargo build --workspace`. Replace `make_sort_options_model`'s literal pushes with `SortOption::all().iter().map(|o| SharedString::from(o.to_string()))`, and `if option_str == "Relevance"` / `match "Size"/"Type"` with the selected `SortOption` (via index into `all()` or `to_sort_key`). Keep the index↔option helper mapping coherent.

- [ ] **Step 3: Test, lint, commit + strip**
```bash
cargo test -p imgfind-gui sort_option:: && cargo test -p imgfind-gui && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T2. block
git add -A && git commit -m "typecheck(stringly-typed): SortOption enum for GUI selector [T2]"
```

---

### Task 8 (T10): grid-nav index newtypes

**Spec:** `## T10`. **Lens:** newtype. Risk med. (Ephemeral GUI state, no serde.)

**Files:**
- Modify: `imgfind-gui/src/nav.rs:36` (`move_selection`), `imgfind-gui/src/window.rs:38` (`window_range`), `imgfind-gui/src/main.rs:580-588` (callers). Define the newtypes in `nav.rs` (or a small `imgfind-gui/src/grid_index.rs`).

**Interfaces:**
- Produces: `CursorIndex(usize)`, `GridCols(usize)`, `ItemCount(usize)` (`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, each `.get()`).

- [ ] **Step 1: Define the three newtypes and change `move_selection` / `window_range` signatures**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct CursorIndex(pub usize);
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct GridCols(pub usize);
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct ItemCount(pub usize);
```
Apply each to the matching param. `cargo build --workspace`; wrap at call sites, unwrap (`.0`) inside the math.

- [ ] **Step 2: Keep nav/window unit tests green (update call sites only)**

Update the existing `nav.rs`/`window.rs` test call sites to wrap args in the newtypes; do NOT change their assertions.

- [ ] **Step 3: Build, test, lint, commit + strip**
```bash
cargo build --workspace && cargo test -p imgfind-gui && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T10. block
git add -A && git commit -m "typecheck(newtype): grid-nav index newtypes [T10]"
```

---

### Task 9 (T12): `SearchState::Phase` enum

**Spec:** `## T12`. **Lens:** illegal-states. Risk low. (Ephemeral, no serde.)

**Files:**
- Modify: `imgfind-gui/src/state.rs:17-86` (fields + `start_search`/`apply_results`/`apply_error`/`view_state`).

**Interfaces:**
- Consumes: `RowMeta` (now carries `ImageId`/`FileSize` from Tasks 1/2 — no extra work).
- Produces: `enum Phase { Idle, Loading, Complete { results: Vec<RowMeta>, error: Option<String> } }` on `SearchState`.

- [ ] **Step 1: Replace the four fields with `phase: Phase`**

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Phase {
    #[default] Idle,
    Loading,
    Complete { results: Vec<RowMeta>, error: Option<String> },
}
```
Route `start_search` → `Phase::Loading`, `apply_results` → `Phase::Complete { results, error: None }`, `apply_error` → `Phase::Complete { results: vec![], error: Some(e) }`. Rewrite `view_state` to match on `Phase` (Idle→idle, Loading→loading, Complete with empty results & no error→empty, with error→error, else results). Preserve the public method signatures the GUI calls.

- [ ] **Step 2: Compiler-drive + keep state tests green**

`cargo build --workspace`. Update the existing `state.rs` tests to construct/observe via the methods and `view_state` (do not change their intent). Add a comment/test documenting that `loading && error` is now unrepresentable.

- [ ] **Step 3: Build, test, lint, commit + strip**
```bash
cargo build --workspace && cargo test -p imgfind-gui && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T12. block
git add -A && git commit -m "typecheck(illegal-states): SearchState Phase enum [T12]"
```

---

### Task 10 (T11): `ThumbnailSize(u32)` newtype

**Spec:** `## T11`. **Lens:** newtype. Risk low.

**Files:**
- Modify: add `ThumbnailSize` to `src/units.rs`; `src/thumbnail.rs:10, 44, 124, 141`, `src/database.rs:701, 1057, 1095-1098`.

**Interfaces:**
- Produces: `imgfind::units::ThumbnailSize(u32)` — `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]`, `pub const fn get(self) -> u32`.

- [ ] **Step 1: Add the newtype + retype `GUI_THUMBNAIL_SIZES`**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThumbnailSize(pub u32);
impl ThumbnailSize { pub const fn get(self) -> u32 { self.0 } }
```
`GUI_THUMBNAIL_SIZES: [ThumbnailSize; 3]` and `LIGHTBOX_SIZE: ThumbnailSize`.

- [ ] **Step 2: Compiler-drive thumbnail/DB signatures**

`cargo build --workspace`; the `thumbnails (image_hash, size)` bind unwraps via `.get()`. Fix every break.

- [ ] **Step 3: Build, test, lint, commit + strip**
```bash
cargo build --workspace && cargo test --workspace && cargo fmt --all && cargo clippy --workspace --all-targets
# strip ### T11. block (last finding under ## Low)
git add -A && git commit -m "typecheck(newtype): ThumbnailSize pixel newtype [T11]"
```

---

### Final verification
- [ ] `cargo test --workspace` green (batch-end milestone).
- [ ] `cargo clippy --workspace --all-targets` no new warnings; `cargo fmt --all` clean.
- [ ] `TYPECHECK.md` retains only T8 and T9 (+ headers); T1–T7, T10–T12 stripped.

## Self-Review notes
- **Spec coverage:** T1→Task1, T7→Task2, T3→Task3, T4→Task4, T5→Task5, T6→Task6, T2→Task7, T10→Task8, T12→Task9, T11→Task10. All ten selected; T8/T9 out of scope.
- **Ordering rationale:** T1 first (widest, touches RowMeta which T12 holds); T7 before T3 (both touch Filters; T7 mechanical); T3 carries the serde-shim risk + decision-needed escape; value newtypes (T5/T6/T11) and GUI enums (T2/T10/T12) after.
- **Persisted-JSON invariants pinned:** T1 (transparent ids + assert bare-int JSON), T3 (old-flat-blob deserialize tests).
- **Type consistency:** newtype accessors named `.get()` (ids/MaxK/EmbeddingDim/ThumbnailSize) and `.bytes()` (FileSize); `ImageId/TagId/CollectionId` in `src/ids.rs`, value newtypes in `src/units.rs`, both re-exported from `src/lib.rs`.
