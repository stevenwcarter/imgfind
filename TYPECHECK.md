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
