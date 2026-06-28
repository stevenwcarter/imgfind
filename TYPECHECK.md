# TYPECHECK.md — type-system strengthening findings

Last triage: 2026-06-28 against `main` @ 2e76bb22. Toolchain: cargo build --workspace / cargo check --workspace / cargo test --workspace / cargo clippy --workspace --all-targets.

> **For future sessions reading this file:** when you fix an item listed
> here, strip it from this file in the same commit that fixes it. The list
> is intended to reflect open issues only; resolved items shouldn't linger.
> This keeps the file's signal-to-noise high for the next typecheck pass.

## How to use this file
- Check `[x] execute` on items to run this batch.
- Check `[x] skip` on items to never re-flag (the skill records them in user memory).
- Items left unchecked stay in TYPECHECK.md for the next run.
- Ranking is impact = bug-prevention × blast-radius (effort is shown separately, never folded into the rank).
- When ready, run `/typecheck --execute`.

_IDs continue from T10: T1–T9 were resolved in prior passes (the last, T8/T9, are referenced by SHA in git history), so new findings keep climbing rather than reusing those numbers._

## Critical

_(none)_

## High

### T10. Pagination `limit`/`offset` are interchangeable `usize` params: `Database::search_similar_images_meta(.., limit, offset, ..)` (src/database.rs:896)
- Lens: newtype
- Impact: 12 (bug-prevention 4 × blast-radius 3)
- Effort: L (≈8–10 sites across 3 files, public-API: yes)
- Risk: medium
- Blast radius: src/database.rs:896 (`search_similar_images_with_raw_blob`), :941 (`search_similar_images_meta`), :988 (`find_similar_to_path`), :1400 (`browse`), :901/:947 (clamp + offset arithmetic); src/search.rs:50,70; src/main.rs:245,266. Existing guard test: src/database.rs:2141 (`search_meta_paginates_past_first_page`).
- Proposed type: `pub struct Limit(pub usize);` + `pub struct Offset(pub usize);` in `src/units.rs` (both `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, `const fn get()`, no serde — not persisted). Change the four DB search/browse signatures and their `SearchEngine`/CLI callers; `.get()` only at the SQL `LIMIT`/`OFFSET` boundary. Swapping the two args becomes a compile error. **Caveat:** these are adjacent same-primitive params (the canonical mixup hazard), but a swap degrades to wrong-page results, not corruption/security — hence High, not Critical. The existing pagination test uses asymmetric values and would catch an obvious swap today; the newtype guards future callers that don't.
- [ ] execute   [ ] skip

## Medium

### T11. EXIF GPS hemisphere reference is a bare `&str` matched on `"S"`/`"W"`: `parse_gps_coordinate(.., reference: &str)` (src/database.rs:1742)
- Lens: stringly-enum
- Impact: 8 (bug-prevention 4 × blast-radius 2)
- Effort: S (≈5 sites, one file, public-API: no — private fn)
- Risk: low
- Blast radius: src/database.rs:1742 (signature), :1755 (`reference == "S" || reference == "W"` sign flip), :1722–1727 (call site passing `lat_ref`/`lon_ref` `display_value().to_string()`).
- Proposed type: `enum GpsRef { North, South, East, West }` with `GpsRef::from_exif(&str) -> Option<Self>` (or `FromStr`) and an `is_negative()` predicate (true for South/West). Parse the EXIF ref string once at the call site; a typo/unknown ref currently falls through `== "S"`/`"W"` as a *silent no-op* (treated as positive) — `from_exif` returning `None` makes the unhandled case explicit. Internal parsing only; no serde/DB-string boundary to preserve. Value set: `["N","S","E","W"]`.
- [ ] execute   [ ] skip

### T12. `ImageWithMetadata` GPS lat/long are unpaired `Option`s (half-coordinate is constructible): `ImageWithMetadata.latitude + longitude` (src/database.rs:1657)
- Lens: illegal-states
- Impact: 8 (bug-prevention 4 × blast-radius 2)
- Effort: M (≈5 sites, one file, public-API: yes — public struct field)
- Risk: low (a half-coordinate characterization test already exists at src/database.rs:1930)
- Blast radius: src/database.rs:1653–1661 (struct def), :1369–1370 (construction in `get_images_by_bounds`), :1776 (`downsample_by_grid` `if let (Some, Some)`), :1802 + :1805–1806 (`apply_stable_jitter` defensive match + mutation).
- Proposed type: replace `latitude: Option<f64>, longitude: Option<f64>` with `coords: Option<GpsCoords>` — **`GpsCoords` already exists** (src/database.rs:1634) and `ImageMetadata` already uses exactly this `coords: Option<GpsCoords>` pattern (:1645, with a doc-comment stating lat/long are always present together). This is a consistency migration onto an established in-repo type: the two `if let (Some, Some)` defensive checks collapse to `if let Some(GpsCoords { lat, lon })`, and the mutation site updates `coords` as a unit. Deletes the `Some(lat), None` / `None, Some(lon)` half-present states.
- [ ] execute   [ ] skip

### T13. Embedding `Vec<f32>` enters the insert boundary without a dimension check: `Database::insert_images_batch(rows: &[(String, String, Vec<f32>)])` (src/database.rs:749)
- Lens: parse-dont-validate
- Impact: 6 (bug-prevention 3 × blast-radius 2)
- Effort: M (≈6 call sites, public-API: yes)
- Risk: medium
- Blast radius: src/database.rs:749 (`insert_images_batch`), :255 (`insert_image`); src/main.rs:615; imgfind-gui/src/backend.rs:318,358,384,459,496,536; tests/cli_smoke.rs:46.
- Proposed type: a `ValidatedEmbedding(Vec<f32>)` produced by `Database::validate_embedding(dim: EmbeddingDim, v: Vec<f32>) -> Result<ValidatedEmbedding>` (length must equal the active model's `EmbeddingDim`). Change the insert signatures to take `ValidatedEmbedding` so a wrong-dimension vector can't reach the `F32_BLOB(dim)` column. **Tradeoff:** normal paths already produce correct-dim embeddings from `clipper`, so this guards a programming-error path (mixing models) rather than a live bug — hence the lower impact; weigh against the M-effort signature churn. Alternative: a single runtime `ensure!` at the boundary (cheaper, no type change) if the compile-time guarantee isn't worth the churn.
- [ ] execute   [ ] skip

## Low

_(none)_

## Skip (do not re-flag in future runs)
_(none)_
