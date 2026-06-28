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
- [x] execute   [ ] skip
