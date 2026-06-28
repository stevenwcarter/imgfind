# typecheck execution: GeoRect + DistanceThreshold newtypes

Date: 2026-06-28
Source: `TYPECHECK.md` items **T8** and **T9** (both `[x] execute`).
Lens: newtype (both). Toolchain: `cargo build/check --workspace`,
`cargo test --workspace`, `cargo clippy --workspace --all-targets`,
`cargo fmt --all`.

This spec covers exactly the two selected findings. Each is a single-commit,
compiler-driven newtype migration. Neither is `risk: high`, so no
characterization tests are required by the per-task contract; both nonetheless
gain a focused unit test for the invariant they introduce.

## Invariants this work depends on

- `get_images_by_bounds` has **zero callers** today (no map view yet) — verified
  by grep across `src/`, `imgfind-gui/`, `imgfind-launcher/`. The compiler is
  therefore the complete to-do list for T8; if a caller appears the migration
  still surfaces it.
- `config.toml`'s `[search].distance_threshold` is persisted as a bare float;
  T9's newtype must keep that on-disk representation (`#[serde(transparent)]`,
  mirroring the existing `MaxK`/`FileSize` newtypes in `src/units.rs`).

---

## T8 — `GeoRect` newtype for geographic bounds

**Symbol:** `Database::get_images_by_bounds(north, south, east, west: f64)` in
`src/database.rs` (~line 1326).

**Problem:** four adjacent `f64` params; transposing N/S or E/W compiles and
silently queries the wrong rectangle.

**Proposed type:** a struct in `src/units.rs`:

```rust
/// An axis-aligned geographic query rectangle (degrees). A struct so the four
/// edges of a map-view bounds query can't be transposed positionally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoRect {
    pub north: f64,
    pub south: f64,
    pub east: f64,
    pub west: f64,
}
```

- Add a `GeoRect::new(north, south, east, west)` constructor that normalizes the
  corners so `south <= north` and `west <= east` (the function already does
  `min`/`max` internally — fold that into the constructor and expose
  `lat_low`/`lat_high`/`long_low`/`long_high` accessors, or keep the
  min/max at the query site). Keep it simple: the constructor stores the four
  edges as given; expose `lat_low()/lat_high()/long_low()/long_high()` helpers
  returning the min/max so the SQL site stops doing it inline.
- No serde (these bounds come from a live map viewport, not from any persisted
  struct) — matches `EmbeddingDim`'s "plain, no serde" precedent.

**Migration:** change the signature to take `GeoRect`; update the body to read
the edges via the accessors; add a unit test asserting the accessors return the
normalized low/high regardless of corner order. No call sites to fix (none
exist).

**Commit:** `typecheck(newtype): GeoRect bounds for get_images_by_bounds [T8]`
(strip T8 from `TYPECHECK.md` in the same commit).

---

## T9 — `DistanceThreshold(f32)` newtype

**Symbol:** the `distance_threshold: f32` parameter threaded through
`Database::search_similar_images`, `search_similar_images_with_raw_blob`,
`search_similar_images_meta` (`src/database.rs`), sourced from
`SearchConfig.distance_threshold` (`src/config.rs:35`) and passed from
`imgfind-gui/src/backend.rs` (and `src/search.rs` / CLI).

**Problem:** documentation-grade — a bare `f32` for a cosine distance in
`[0, 2]` (threshold ≤ 1.3). Transposition is already prevented (adjacent params
are different primitives), so the value is semantic clarity + a range invariant,
mirroring the sibling `MaxK`.

**Proposed type:** in `src/units.rs`, beside `MaxK`:

```rust
/// A cosine-distance cutoff for KNN search. Cosine distance lives in [0, 2];
/// the configured default is 1.3. `#[serde(transparent)]` keeps the persisted
/// `SearchConfig.distance_threshold` a bare float in config.toml.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DistanceThreshold(pub f32);

impl DistanceThreshold {
    pub const fn get(self) -> f32 { self.0 }
}
```

**Migration:** change `SearchConfig.distance_threshold` and the three
`Database` search method params to `DistanceThreshold`; let the compiler
enumerate every break (`config.rs` default + tests, `backend.rs`, `search.rs`,
`main.rs`, `vector_sql::knn_query` consumes the inner `f32` via `.get()`,
database.rs tests passing `1.3` literals become `DistanceThreshold(1.3)`); fix
to green. Add a serde round-trip unit test (bare-float on disk) mirroring the
existing `MaxK` test in `src/units.rs`.

**Commit:** `typecheck(newtype): DistanceThreshold(f32) for KNN cutoff [T9]`
(strip T9 from `TYPECHECK.md` in the same commit).

---

## Verification (controller, between and after each commit)

- `cargo build --workspace` green.
- `cargo clippy --workspace --all-targets` clean.
- `cargo fmt --all --check` clean (run `cargo fmt --all` first — subagents drift).
- `cargo test --workspace` green (full suite at the end, both findings being the
  whole batch).
