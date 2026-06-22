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

## Medium

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
