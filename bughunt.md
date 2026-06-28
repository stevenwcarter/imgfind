# bughunt.md — code-health audit findings

Last triage: 2026-06-20 against `main` @ de6dcd24. Toolchain: cargo build --workspace / cargo test --workspace / cargo clippy --workspace --all-targets.

> **For future sessions reading this file:** when you fix an item listed
> here, strip it from this file in the same commit that fixes it. The list
> is intended to reflect open issues only; resolved items shouldn't linger.
> This keeps the file's signal-to-noise high for the next audit pass.

## How to use this file
- Check `[x] execute` on items to fix this batch.
- Check `[x] skip` on items to never re-flag (the skill records them in user memory).
- Items left unchecked stay in bughunt.md for the next run.
- Ranking is impact = severity × blast-radius (effort is shown separately, never folded into the rank).
- When ready, run `/code-health --execute`.

## Critical

_(none)_

## High

## Medium

## Low

### B15. Open-in-OS-viewer failure only logged, no UI feedback: `on_tile_open_external` (imgfind-gui/src/main.rs:744)
- Category: api-surface
- Impact: 4 (severity 2 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `open::that(&abs)` errors on right-click-open are caught and `tracing::warn!`'d; the GUI shows nothing, so the user gets no indication the open failed.
- Blast radius: imgfind-gui/src/main.rs:744
- Proposed fix: On `Err`, surface a transient UI error (toast/status field).
- [x] execute   [ ] skip

## Skip (do not re-flag in future runs)
- `SearchState.results` unbounded `Vec<RowMeta>` at imgfind-gui/src/state.rs:19 — search results are 100 by default and only "relevance" is in-memory sorted; O(100) is fine.
- ANSI escape codes in file logs via `with_ansi(true)` at src/logging.rs:29 (B9) — skipped 2026-06-28.
